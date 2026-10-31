//! C1: refuses 2 `sandbox` when the caller runs inside a sandbox (Unix.ps1:113-123, MuxAgents.ps1:903-910).

use crate::ctx::Ctx;
use crate::error::{Res, refuse};
use crate::proc::{Signals, Token, windows};

fn sandboxed<T>(message: &str) -> Res<T> {
    refuse(2, "sandbox", message)
}

/// Every OS: SANDBOX_RUNTIME=1, exactly, refuses.
pub fn decide_runtime(value: Option<&str>) -> Res<()> {
    match value {
        Some("1") => sandboxed("SANDBOX_RUNTIME=1 is set"),
        _ => Ok(()),
    }
}

/// Linux: the first `NoNewPrivs:` line holding only blanks around digits decides (Unix.ps1:113-118); a line
/// ending in CR is not one, as in the reference's multiline `$`.
pub fn decide_status(status: Option<&str>) -> Res<()> {
    let Some(status) = status else {
        return sandboxed("/proc/self/status is unreadable");
    };
    let value = status
        .split('\n')
        .filter_map(|line| line.strip_prefix("NoNewPrivs:"))
        .map(|rest| rest.trim_matches([' ', '\t']))
        .find(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()));
    match value {
        None => sandboxed("NoNewPrivs is unreadable"),
        // Compared as text, like the reference: "00" is set.
        Some("0") => Ok(()),
        Some(_) => sandboxed("NoNewPrivs is set"),
    }
}

/// macOS: sandbox_check(getpid(), NULL, 0) is 0 only outside a sandbox (Unix.ps1:119-123).
pub fn decide_check(result: i32) -> Res<()> {
    if result == 0 {
        Ok(())
    } else {
        sandboxed("sandbox_check reports a sandbox")
    }
}

/// Windows: an unreadable, AppContainer or restricted token, or one below Medium integrity (MuxAgents.ps1:903-910).
pub fn decide_token(token: Option<Token>) -> Res<()> {
    if windows::token_admits(token) {
        Ok(())
    } else {
        sandboxed("the caller runs in a sandbox or below Medium integrity")
    }
}

pub fn decide(signals: &Signals) -> Res<()> {
    match signals {
        Signals::SandboxCheck(result) => decide_check(*result),
        Signals::Status(status) => decide_status(status.as_deref()),
        Signals::Token(token) => decide_token(*token),
    }
}

/// C1, before any multiplexer call. An admitted launcher pane passed it when spawned, so nothing is read; else
/// the variable first, then the OS.
pub fn assert_unsandboxed(ctx: &Ctx) -> Res<()> {
    if ctx.admitted {
        return Ok(());
    }
    decide_runtime(ctx.env.get("SANDBOX_RUNTIME"))?;
    decide(&ctx.os.sandbox())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::Env;
    use crate::testing::Fakes;

    // "code check message", or "none" for a pass.
    fn answer(result: Res<()>) -> String {
        match result {
            Ok(()) => "none".into(),
            Err(r) => format!("{} {} {}", r.code, r.check, r.message),
        }
    }

    fn token(app_container: bool, restricted: bool, integrity_rid: u32) -> Option<Token> {
        Some(Token {
            app_container,
            restricted,
            integrity_rid,
        })
    }

    #[test]
    fn sandbox_runtime_1_refuses() {
        assert_eq!(
            answer(decide_runtime(Some("1"))),
            "2 sandbox SANDBOX_RUNTIME=1 is set"
        );
        // Exactly 1; anything else, or unset, is no signal.
        for value in [None, Some("0"), Some(""), Some(" 1"), Some("true")] {
            assert_eq!(answer(decide_runtime(value)), "none", "{value:?}");
        }
    }

    #[test]
    fn nonewprivs_set_refuses() {
        for status in [
            "Name:\tx\nNoNewPrivs:\t1\n",
            "NoNewPrivs: 2",
            "NoNewPrivs:\t00\n",
        ] {
            assert_eq!(
                answer(decide_status(Some(status))),
                "2 sandbox NoNewPrivs is set",
                "{status:?}"
            );
        }
        for status in [
            "Name:\tpwsh\nNoNewPrivs:\t0\n",
            "NoNewPrivs:0",
            "NoNewPrivs: \t0 \t\nSeccomp:\t0\n",
        ] {
            assert_eq!(answer(decide_status(Some(status))), "none", "{status:?}");
        }
    }

    #[test]
    fn nonewprivs_unreadable_refuses() {
        assert_eq!(
            answer(decide_status(None)),
            "2 sandbox /proc/self/status is unreadable"
        );
        // No line, no digits, junk after them, a CR before the line end, or the name mid-line.
        for status in [
            "Name:\tx\n",
            "NoNewPrivs:\n",
            "NoNewPrivs:\t0 x\n",
            "NoNewPrivs:\t0\r\n",
            "XNoNewPrivs:\t0\n",
            "",
        ] {
            assert_eq!(
                answer(decide_status(Some(status))),
                "2 sandbox NoNewPrivs is unreadable",
                "{status:?}"
            );
        }
        // The first well-formed line decides.
        assert_eq!(
            answer(decide_status(Some("NoNewPrivs:\tx\nNoNewPrivs:\t1\n"))),
            "2 sandbox NoNewPrivs is set"
        );
    }

    #[test]
    fn sandbox_check_nonzero_refuses() {
        for result in [1, -1, 2] {
            assert_eq!(
                answer(decide_check(result)),
                "2 sandbox sandbox_check reports a sandbox"
            );
        }
        assert_eq!(answer(decide_check(0)), "none");
    }

    #[test]
    fn windows_low_integrity_refuses() {
        let refused = "2 sandbox the caller runs in a sandbox or below Medium integrity";
        for t in [
            None,
            token(true, false, 8192),
            token(false, true, 8192),
            token(false, false, 4096),
        ] {
            assert_eq!(answer(decide_token(t)), refused, "{t:?}");
        }
        assert_eq!(answer(decide_token(token(false, false, 8192))), "none");
        assert_eq!(answer(decide_token(token(false, false, 12288))), "none");
    }

    #[test]
    fn decide_reads_each_os_signal() {
        assert_eq!(answer(decide(&Signals::SandboxCheck(0))), "none");
        assert_eq!(
            answer(decide(&Signals::SandboxCheck(1))),
            "2 sandbox sandbox_check reports a sandbox"
        );
        assert_eq!(
            answer(decide(&Signals::Status(Some("NoNewPrivs:\t1\n".into())))),
            "2 sandbox NoNewPrivs is set"
        );
        assert_eq!(
            answer(decide(&Signals::Token(token(false, false, 4096)))),
            "2 sandbox the caller runs in a sandbox or below Medium integrity"
        );
    }

    #[test]
    fn admitted_skips() {
        let fakes = Fakes::default();
        fakes.signals(Signals::SandboxCheck(1));
        let mut ctx = fakes.ctx();
        ctx.env = Env::from_pairs([("SANDBOX_RUNTIME", "1")]);
        ctx.admitted = true;
        // The launcher's pane passed C1 when it was spawned: nothing is read.
        assert_eq!(answer(assert_unsandboxed(&ctx)), "none");
        assert!(fakes.os_calls().is_empty());

        // Not admitted: the variable refuses before the OS is asked, then the OS signal refuses.
        ctx.admitted = false;
        assert_eq!(
            answer(assert_unsandboxed(&ctx)),
            "2 sandbox SANDBOX_RUNTIME=1 is set"
        );
        assert!(fakes.os_calls().is_empty());
        ctx.env = Env::default();
        assert_eq!(
            answer(assert_unsandboxed(&ctx)),
            "2 sandbox sandbox_check reports a sandbox"
        );
        assert_eq!(fakes.os_calls(), ["sandbox"]);
        fakes.signals(Signals::SandboxCheck(0));
        assert_eq!(answer(assert_unsandboxed(&ctx)), "none");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_check_of_self_is_zero() {
        assert_eq!(crate::proc::host().sandbox(), Signals::SandboxCheck(0));
        assert_eq!(answer(decide(&crate::proc::host().sandbox())), "none");
    }
}
