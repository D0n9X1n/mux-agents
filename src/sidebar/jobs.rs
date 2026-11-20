//! The bar's slow work off its loop: a resume, and opening another view window's bar (a split that may wait on the
//! session's sidebar lock), each run on a worker thread while the bar keeps drawing; the loop collects the answers on
//! a later pass without ever waiting for them. One action at a time per row, one open at a time per window.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};

use serde_json::{Map, Value};

use super::pick::Target;
use crate::caller::Caller;
use crate::ctx::{Ctx, Env};
use crate::error::{Refusal, Res};

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
    // Under test the work runs on the bar's own thread at the next drain: the test fakes are `Rc`, so they cannot
    // reach a worker. The frame drawn before that drain is the one a worker leaves the bar free to draw.
    #[cfg(test)]
    deferred: Vec<Target>,
    #[cfg(test)]
    deferred_windows: Vec<String>,
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
        for target in std::mem::take(&mut self.deferred) {
            let answer = crate::agents::resume::resume_by_id(ctx, caller, &target.id, true, false);
            let _ = self.send.send((target, answer));
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
