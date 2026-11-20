//! The bar's slow verbs off its loop: a resume runs on a worker thread while the bar keeps drawing, and the loop collects
//! its answer on a later pass without ever waiting for it. One action at a time per row.

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

/// The actions running (by row id) and the channel their answers come back on.
#[derive(Debug)]
pub(crate) struct Jobs {
    pending: BTreeSet<String>,
    send: Sender<Done>,
    recv: Receiver<Done>,
    // Under test the verb runs on the bar's own thread at the next drain: the test fakes are `Rc`, so they cannot
    // reach a worker. The frame drawn before that drain is the one a worker leaves the bar free to draw.
    #[cfg(test)]
    deferred: Vec<Target>,
}

impl Default for Jobs {
    fn default() -> Self {
        let (send, recv) = channel();
        Jobs {
            pending: BTreeSet::new(),
            send,
            recv,
            #[cfg(test)]
            deferred: Vec::new(),
        }
    }
}

impl Jobs {
    /// Resumes ended member `target` by id, as a tab, off the loop; false (nothing started) while one is pending for it.
    pub(crate) fn resume(&mut self, ctx: &Ctx, caller: &Caller, target: &Target) -> bool {
        if !self.pending.insert(target.id.clone()) {
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
        &self,
        target: Target,
        job: impl FnOnce() -> Res<Map<String, Value>> + Send + 'static,
    ) {
        let (send, failed) = (self.send.clone(), (self.send.clone(), target.clone()));
        let spawned = std::thread::Builder::new()
            .name("mux-agents-bar-job".into())
            .spawn(move || {
                let answer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job))
                    .unwrap_or_else(|_| Err(Refusal::internal("panic")));
                let _ = send.send((target, answer));
            });
        if spawned.is_err() {
            let (send, target) = failed;
            let _ = send.send((target, Err(Refusal::internal("no worker thread"))));
        }
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
