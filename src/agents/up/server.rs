//! Before each multiplexer step, the server's pid and start are still the caller's.

use crate::{
    caller::Caller,
    ctx::Ctx,
    error::{Refusal, Res},
    mux::native,
};

pub const RESTARTED: &str = "the multiplexer restarted; run up again";

pub fn same(ctx: &Ctx, dest: &Caller) -> Res<()> {
    let restarted = || Refusal {
        code: 1,
        check: "endpoint",
        message: RESTARTED.into(),
        reason: None,
        leftover: None,
    };
    let out = native(
        ctx,
        &["display-message", "-p", "-t", &dest.session, "up #{pid}"],
        true,
    )
    .map_err(|_| restarted())?;
    let pid = out
        .stdout
        .trim_end_matches(['\r', '\n'])
        .strip_prefix("up ")
        .and_then(|p| p.parse::<u32>().ok());
    let start = ctx.os.open(dest.server_pid).map(|o| o.start);
    let same = out.code == 0
        && pid == Some(dest.server_pid)
        && start.is_some_and(|s| ctx.os.same_start(s, dest.server_start));
    if same { Ok(()) } else { Err(restarted()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_server_passes_and_a_new_pid_or_start_refuses() {
        let w = crate::agents::fixture::World::human("");
        let c = w.caller();
        let pid = std::rc::Rc::new(std::cell::Cell::new(c.server_pid));
        let p = pid.clone();
        w.fakes.mux_script("up #{pid}", move |_| {
            Ok(crate::mux::Output {
                code: 0,
                stdout: format!("up {}\n", p.get()),
                stderr: String::new(),
            })
        });
        same(&w.ctx, &c).unwrap();
        w.fakes.opens(c.server_pid, &[Some(c.server_start + 1)]);
        assert_eq!(same(&w.ctx, &c).unwrap_err().message, RESTARTED);
        pid.set(c.server_pid + 1);
        assert_eq!(same(&w.ctx, &c).unwrap_err().message, RESTARTED);
    }
}
