//! The bar's slow work off its loop: one serial reader for boards and seen marks, plus resumes and opening other
//! windows' bars. The loop collects answers without waiting: one board read in flight, one action per row, one open
//! per window. Shutdown discards unclaimed reader work and joins its claimed request before restoring input.

use std::cell::RefCell;
use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};

use serde_json::{Map, Value};

use super::pick::Target;
use crate::caller::Caller;
use crate::ctx::{Ctx, Env};
use crate::error::{Refusal, Res};

/// The identity of a viewed spell survives a moved or missing row while its mark runs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Seen {
    pub id: String,
    pub pane: String,
    pub changed: String,
}

/// Work for the one serial board reader. Seen writes queued before a read finish before it starts.
#[derive(Debug)]
pub(crate) enum Request {
    Read(u64),
    Seen(Seen),
}

impl Request {
    fn refused(&self) -> Answer {
        match self {
            Self::Read(generation) => {
                Answer::Read(*generation, Err(Refusal::internal("board reader failed")))
            }
            Self::Seen(seen) => Answer::Seen(seen.clone(), false),
        }
    }
}

#[derive(Debug)]
pub(crate) enum Answer {
    Read(u64, Res<Map<String, Value>>),
    Seen(Seen, bool),
}

type Queue = Arc<(Mutex<(VecDeque<Request>, bool)>, Condvar)>;

/// Claiming work and stopping share a lock: closing a bar never starts a queued seen write.
#[derive(Debug)]
struct Reader {
    queue: Queue,
    answers: Receiver<(Answer, bool)>,
    send: Sender<(Answer, bool)>,
    failed: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Reader {
    fn start<F: FnMut(&Request) -> Answer + 'static>(
        make: impl FnOnce() -> F + Send + 'static,
    ) -> Self {
        Self::start_with(make, |run| {
            std::thread::Builder::new()
                .name("mux-agents-bar-reader".into())
                .spawn(run)
        })
    }

    fn start_with<F: FnMut(&Request) -> Answer + 'static>(
        make: impl FnOnce() -> F + Send + 'static,
        spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<std::thread::JoinHandle<()>>,
    ) -> Self {
        Self::start_between(make, spawn, || {})
    }

    fn start_between<F: FnMut(&Request) -> Answer + 'static>(
        make: impl FnOnce() -> F + Send + 'static,
        spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<std::thread::JoinHandle<()>>,
        mut between: impl FnMut() + Send + 'static,
    ) -> Self {
        let queue: Queue = Arc::new((Mutex::new((VecDeque::new(), false)), Condvar::new()));
        let work = queue.clone();
        let (send, answers) = channel();
        let worker = send.clone();
        let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let broken = failed.clone();
        let handle = spawn(Box::new(move || {
            let mut run = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(make)) {
                Ok(run) => run,
                Err(_) => {
                    let mut state = work.0.lock().unwrap();
                    broken.store(true, std::sync::atomic::Ordering::SeqCst);
                    for queued in state.0.drain(..) {
                        let _ = worker.send((queued.refused(), true));
                    }
                    return;
                }
            };
            loop {
                let request = {
                    let (lock, ready) = &*work;
                    let mut state = lock.lock().unwrap();
                    while state.0.is_empty() && !state.1 {
                        state = ready.wait(state).unwrap();
                    }
                    if state.1 {
                        return;
                    }
                    state.0.pop_front().unwrap()
                };
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&request))) {
                    Ok(answer) => {
                        let _ = worker.send((answer, false));
                        between();
                    }
                    Err(_) => {
                        let mut state = work.0.lock().unwrap();
                        broken.store(true, std::sync::atomic::Ordering::SeqCst);
                        let _ = worker.send((request.refused(), true));
                        for queued in state.0.drain(..) {
                            let _ = worker.send((queued.refused(), true));
                        }
                        return;
                    }
                }
            }
        }))
        .ok();
        if handle.is_none() {
            failed.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        Self {
            queue,
            answers,
            send,
            failed,
            handle,
        }
    }

    fn ask(&mut self, request: Request) {
        let (lock, ready) = &*self.queue;
        let mut state = lock.lock().unwrap();
        if !state.1 {
            if self.failed.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = self.send.send((request.refused(), true));
            } else {
                state.0.push_back(request);
                ready.notify_one();
            }
        }
    }

    fn finish(&mut self) {
        let (lock, ready) = &*self.queue;
        {
            let mut state = lock.lock().unwrap();
            state.1 = true;
            state.0.clear();
            ready.notify_all();
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// A finished action: the row it was for, and the verb's answer.
pub(crate) type Done = (Target, Res<Map<String, Value>>);

/// A finished open of another window's bar: the window, and whether its bar is there.
pub(crate) type Ensured = (String, Res<()>);

/// The actions running (by row id) and the windows whose bar is being opened, each with the channel its answers come
/// back on.
#[derive(Debug)]
pub(crate) struct Jobs {
    pending: BTreeSet<String>,
    send: Sender<Done>,
    recv: Receiver<Done>,
    windows: BTreeSet<String>,
    opened: Sender<Ensured>,
    answers: Receiver<Ensured>,
    // Each worker still to be waited for, and whether the watch has stopped taking new work (`finish`).
    handles: Vec<std::thread::JoinHandle<()>>,
    closed: bool,
    reader: Option<Reader>,
    reader_failures: u64,
    reading: bool,
    seeing: BTreeSet<Seen>,
    generation: u64,
    ready: VecDeque<(Answer, bool)>,
    // Under test the work runs on the bar's own thread at the next drain: the test fakes are `Rc`, so they cannot
    // reach a worker. The frame drawn before that drain is the one a worker leaves the bar free to draw.
    #[cfg(test)]
    deferred: Vec<Target>,
    #[cfg(test)]
    deferred_windows: Vec<String>,
}

#[cfg(test)]
std::thread_local! {
    // Under test, while true, deferred resumes are held back: a test releases them to answer at a moment it picks.
    pub(crate) static HOLD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    // A read executes immediately on the fake-provider thread, but its answer can remain in flight.
    pub(crate) static HOLD_READ: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static TEST_READER: RefCell<Option<Reader>> = const { RefCell::new(None) };
    static TEST_FACTORY: RefCell<Option<Box<dyn FnMut() -> Reader>>> = const { RefCell::new(None) };
}

/// A real worker for a watch test; only its closures leave the fake providers' thread.
#[cfg(test)]
pub(super) fn test_reader<F: FnMut(&Request) -> Answer + 'static>(
    make: impl FnOnce() -> F + Send + 'static,
) -> Queue {
    let reader = Reader::start(make);
    let queue = reader.queue.clone();
    TEST_READER.with(|slot| *slot.borrow_mut() = Some(reader));
    queue
}

#[cfg(test)]
pub(super) fn test_idle_reader() -> Queue {
    let (ready, idle) = channel();
    let queue = test_reader(move || {
        ready.send(()).unwrap();
        |_: &Request| panic!("idle reader must claim nothing")
    });
    idle.recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    queue
}

#[cfg(test)]
pub(super) fn test_reader_between<F: FnMut(&Request) -> Answer + 'static>(
    make: impl FnOnce() -> F + Send + 'static,
    between: impl FnMut() + Send + 'static,
) -> Queue {
    let reader = Reader::start_between(make, |run| std::thread::Builder::new().spawn(run), between);
    let queue = reader.queue.clone();
    TEST_READER.with(|slot| *slot.borrow_mut() = Some(reader));
    queue
}

#[cfg(test)]
pub(super) fn test_reader_failures(start_failure: bool, mut started: impl FnMut() + 'static) {
    TEST_FACTORY.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            started();
            if start_failure {
                Reader::start_with(
                    || |_: &Request| panic!("no thread"),
                    |_| Err(std::io::Error::other("no reader thread")),
                )
            } else {
                Reader::start(|| |_: &Request| panic!("reader request panic"))
            }
        }))
    });
}

#[cfg(test)]
pub(super) fn clear_test_reader_factory() {
    TEST_FACTORY.with(|slot| *slot.borrow_mut() = None);
}

impl Default for Jobs {
    fn default() -> Self {
        let (send, recv) = channel();
        let (opened, answers) = channel();
        Jobs {
            pending: BTreeSet::new(),
            send,
            recv,
            windows: BTreeSet::new(),
            opened,
            answers,
            handles: Vec::new(),
            closed: false,
            #[cfg(not(test))]
            reader: None,
            #[cfg(test)]
            reader: TEST_READER.with(|slot| slot.borrow_mut().take()),
            reader_failures: 0,
            reading: false,
            seeing: BTreeSet::new(),
            generation: 0,
            ready: VecDeque::new(),
            #[cfg(test)]
            deferred: Vec::new(),
            #[cfg(test)]
            deferred_windows: Vec::new(),
        }
    }
}

/// Runs `job` on a worker thread and sends `(key, answer)` on `send`; a panic, or a thread that could not start,
/// answers a refusal, so nothing stays pending for ever. The worker's handle, for `finish` to wait for.
fn spawn<K: Clone + Send + 'static, T: Send + 'static>(
    send: &Sender<(K, Res<T>)>,
    key: K,
    job: impl FnOnce() -> Res<T> + Send + 'static,
) -> Option<std::thread::JoinHandle<()>> {
    let (worker, failed) = (send.clone(), (send.clone(), key.clone()));
    let spawned = std::thread::Builder::new()
        .name("mux-agents-bar-job".into())
        .spawn(move || {
            let answer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job))
                .unwrap_or_else(|_| Err(Refusal::internal("panic")));
            let _ = worker.send((key, answer));
        });
    match spawned {
        Ok(handle) => Some(handle),
        Err(_) => {
            let (send, key) = failed;
            let _ = send.send((key, Err(Refusal::internal("no worker thread"))));
            None
        }
    }
}

impl Jobs {
    /// The newest requested read, so a mark can reject older answers.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn reading(&self) -> bool {
        self.reading
    }

    /// Ask once; fake-provider tests run here, production uses its one long-lived worker.
    pub(crate) fn read(&mut self, ctx: &Ctx, caller: &Caller) {
        if self.closed || self.reading {
            return;
        }
        self.generation += 1;
        self.reading = true;
        #[cfg(test)]
        if self.reader.is_none() && self.reader_failures < 3 {
            self.reader = TEST_FACTORY.with(|slot| slot.borrow_mut().as_mut().map(|make| make()));
        }
        if self.reader_failures >= 3 || (cfg!(test) && self.reader.is_none()) {
            self.ready.push_back((
                Answer::Read(
                    self.generation,
                    crate::agents::board::board(ctx, caller, true, true),
                ),
                false,
            ));
            return;
        }
        self.start_reader(ctx, caller);
        self.reader
            .as_mut()
            .unwrap()
            .ask(Request::Read(self.generation));
    }

    fn start_reader(&mut self, ctx: &Ctx, caller: &Caller) {
        if self.reader.is_none() {
            let (seed, caller) = (Seed::of(ctx), caller.clone());
            self.reader = Some(Reader::start(move || {
                let ctx = seed.host();
                crate::hc::remember_versions();
                move |request: &Request| match request {
                    Request::Read(generation) => Answer::Read(
                        *generation,
                        crate::agents::board::board(&ctx, &caller, true, true),
                    ),
                    Request::Seen(seen) => Answer::Seen(
                        seen.clone(),
                        crate::agents::board::mark_seen(&ctx, &caller, &seen.pane, &seen.changed)
                            .is_ok_and(|ok| ok),
                    ),
                }
            }));
        }
    }

    /// Queue each viewed spell once until it answers; false marks can be retried next tick.
    pub(crate) fn seen(&mut self, ctx: &Ctx, caller: &Caller, seen: &Seen) {
        if self.closed || !self.seeing.insert(seen.clone()) {
            return;
        }
        if self.reader_failures >= 3 || (cfg!(test) && self.reader.is_none()) {
            self.ready.push_back((
                Answer::Seen(
                    seen.clone(),
                    crate::agents::board::mark_seen(ctx, caller, &seen.pane, &seen.changed)
                        .is_ok_and(|ok| ok),
                ),
                false,
            ));
        } else {
            self.start_reader(ctx, caller);
            self.reader
                .as_mut()
                .unwrap()
                .ask(Request::Seen(seen.clone()));
        }
    }

    /// Collect answers without waiting for a request; a failed worker has only refusals left to join.
    pub(crate) fn drain_reader(&mut self) -> Vec<Answer> {
        if let Some(reader) = &self.reader {
            self.ready.extend(reader.answers.try_iter());
        }
        let mut done = Vec::new();
        let mut broken = false;
        for (answer, failed) in std::mem::take(&mut self.ready) {
            #[cfg(test)]
            if HOLD_READ.get() && matches!(answer, Answer::Read(_, _)) {
                self.ready.push_back((answer, failed));
                continue;
            }
            broken |= failed;
            if let Answer::Seen(seen, _) = &answer {
                self.seeing.remove(seen);
            }
            if matches!(answer, Answer::Read(_, _)) {
                self.reading = false;
                if !failed && self.reader_failures < 3 {
                    self.reader_failures = 0;
                }
            }
            done.push(answer);
        }
        if broken && let Some(mut reader) = self.reader.take() {
            self.reader_failures = (self.reader_failures + 1).min(3);
            reader.finish();
            self.ready.extend(reader.answers.try_iter());
            // The worker may have sent queued refusals after the first drain but before the join ended.
            done.extend(self.drain_reader());
        }
        done
    }

    /// Resumes ended member `target` by id, as a tab, off the loop; false (nothing started) while one is pending for it.
    pub(crate) fn resume(&mut self, ctx: &Ctx, caller: &Caller, target: &Target) -> bool {
        if self.closed || !self.pending.insert(target.id.clone()) {
            return false;
        }
        #[cfg(test)]
        {
            let _ = (ctx, caller);
            self.deferred.push(target.clone());
        }
        #[cfg(not(test))]
        {
            let (seed, caller, id) = (Seed::of(ctx), caller.clone(), target.id.clone());
            self.start(target.clone(), move || {
                crate::agents::resume::resume_by_id(&seed.host(), &caller, &id, true, false)
            });
        }
        true
    }

    /// Runs `job` for `target` on a worker thread; its answer, or a panic or a thread that could not start as a refusal,
    /// comes back on the channel.
    pub(crate) fn start(
        &mut self,
        target: Target,
        job: impl FnOnce() -> Res<Map<String, Value>> + Send + 'static,
    ) {
        self.handles.retain(|h| !h.is_finished());
        self.handles.extend(spawn(&self.send, target, job));
    }

    /// Opens another view window `window`'s bar off the loop: `sidebar::ensure`, which waits on the session's sidebar
    /// lock as long as another holds it; false (nothing started) while one is pending for that window.
    pub(crate) fn ensure(&mut self, ctx: &Ctx, caller: &Caller, window: &str) -> bool {
        if self.closed || !self.windows.insert(window.to_string()) {
            return false;
        }
        #[cfg(test)]
        {
            let _ = (ctx, caller);
            self.deferred_windows.push(window.to_string());
        }
        #[cfg(not(test))]
        {
            let (seed, caller, at) = (Seed::of(ctx), caller.clone(), window.to_string());
            self.handles.retain(|h| !h.is_finished());
            self.handles
                .extend(spawn(&self.opened, window.to_string(), move || {
                    super::ensure(&seed.host(), &caller, &at).map(|_| ())
                }));
        }
        true
    }

    /// The windows whose open answered, without waiting; they are no longer pending.
    pub(crate) fn drain_windows(&mut self, ctx: &Ctx, caller: &Caller) -> Vec<Ensured> {
        #[cfg(test)]
        for window in std::mem::take(&mut self.deferred_windows) {
            let answer = super::ensure(ctx, caller, &window).map(|_| ());
            let _ = self.opened.send((window, answer));
        }
        #[cfg(not(test))]
        let _ = (ctx, caller);
        let done: Vec<Ensured> = self.answers.try_iter().collect();
        for (window, _) in &done {
            self.windows.remove(window);
        }
        done
    }

    /// The answers that have come, without waiting; their rows are no longer pending.
    pub(crate) fn drain(&mut self, ctx: &Ctx, caller: &Caller) -> Vec<Done> {
        #[cfg(test)]
        if !HOLD.with(std::cell::Cell::get) {
            for target in std::mem::take(&mut self.deferred) {
                let answer =
                    crate::agents::resume::resume_by_id(ctx, caller, &target.id, true, false);
                let _ = self.send.send((target, answer));
            }
        }
        #[cfg(not(test))]
        let _ = (ctx, caller);
        let done: Vec<Done> = self.recv.try_iter().collect();
        for (target, _) in &done {
            self.pending.remove(&target.id);
        }
        done
    }

    /// The watch's last step, on every way out of it: no new work starts, then the work still running is waited for,
    /// at most `bound`, so an open is never cut between its split and its mark (a pane left unmarked would be taken
    /// for a watch started by hand) and a resume never between its launch and its record. Their answers are drained:
    /// the marks and records are the work's own, done by then. False when some work did not finish within `bound`.
    pub(crate) fn finish(
        &mut self,
        ctx: &Ctx,
        caller: &Caller,
        bound: std::time::Duration,
    ) -> bool {
        self.closed = true;
        if let Some(reader) = &mut self.reader {
            reader.finish();
        }
        self.ready.clear();
        self.reading = false;
        let end = std::time::Instant::now() + bound;
        // Under test the deferred work runs here, on this thread, as `drain` runs it.
        let _ = (self.drain(ctx, caller), self.drain_windows(ctx, caller));
        while self.handles.iter().any(|h| !h.is_finished()) {
            if std::time::Instant::now() >= end {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
        let _ = (self.drain(ctx, caller), self.drain_windows(ctx, caller));
        true
    }

    /// Whether an action for row `id` is running.
    pub(crate) fn pending(&self, id: &str) -> bool {
        self.pending.contains(id)
    }

    /// `board` with each row whose action runs marked `resuming`, so the bar draws it so from memory until the board
    /// catches up; none while nothing runs (the bar then draws `board` itself).
    pub(crate) fn mark(&self, board: Option<&Map<String, Value>>) -> Option<Map<String, Value>> {
        let mut marked = board.filter(|_| !self.pending.is_empty())?.clone();
        let rows = marked.get_mut("agents").and_then(Value::as_array_mut);
        for row in rows.into_iter().flatten().filter_map(Value::as_object_mut) {
            if self.pending.contains(&Target::of(row).id) {
                row.insert("resuming".into(), true.into());
            }
        }
        Some(marked)
    }
}

/// What a worker builds its own `Ctx` from: the bar's `Ctx` holds providers that cannot leave its thread (the
/// multiplexer it resolved, `RefCell`s), so a worker takes the plain fields and makes fresh host providers, as `main`
/// does. The verb then reads and locks the same state; its locks are file locks, which hold between threads too.
pub(crate) struct Seed {
    env: Env,
    pid: u32,
    cwd: PathBuf,
    home: PathBuf,
    state_root: PathBuf,
    exe: PathBuf,
    member_mux: crate::mux::MemberMux,
    admitted: bool,
    tty: bool,
}

impl Seed {
    pub(crate) fn of(ctx: &Ctx) -> Seed {
        Seed {
            env: ctx.env.clone(),
            pid: ctx.pid,
            cwd: ctx.cwd.clone(),
            home: ctx.home.clone(),
            state_root: ctx.state_root.clone(),
            exe: ctx.exe.clone(),
            member_mux: ctx.member_mux.borrow().clone(),
            admitted: ctx.admitted,
            tty: ctx.tty,
        }
    }

    /// A `Ctx` of the host's own providers over the bar's fields.
    pub(crate) fn host(&self) -> Ctx {
        Ctx {
            env: self.env.clone(),
            pid: self.pid,
            cwd: self.cwd.clone(),
            home: self.home.clone(),
            state_root: self.state_root.clone(),
            exe: self.exe.clone(),
            os: crate::proc::host(),
            mux: RefCell::new(None),
            member_mux: RefCell::new(self.member_mux.clone()),
            hc: Box::new(crate::hc::NativeHc),
            clock: Box::new(crate::ctx::SystemClock),
            admitted: self.admitted,
            tty: self.tty,
        }
    }
}

#[cfg(test)]
mod tests;
