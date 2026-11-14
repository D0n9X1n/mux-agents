use super::{
    registry,
    spawn::{Created, created_pane, utf8},
};
use crate::{
    caller::Caller,
    ctx::Ctx,
    error::{Refusal, Res},
    hc::Core,
    mux::native,
};
use serde_json::Value;
pub fn attach(
    ctx: &Ctx,
    core: &Core<'_>,
    caller: &Caller,
    record: &Value,
    split: Option<&str>,
    pane: &mut Option<Created>,
) -> Res<()> {
    let field = |key| {
        record
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| Refusal::internal("invalid attachment identity"))
    };
    if !super::agent_id(field("id")?)
        || !super::agent_name(field("name")?)
        // A paneless caller (`up`'s leader, #103) may only open a window: a split needs a pane to split.
        || !(super::pane_id(&caller.pane) || (caller.pane.is_empty() && split.is_none()))
    {
        return Err(Refusal::internal("invalid attachment identity"));
    }
    let (address, directory, name) = (field("hcAddress")?, field("directory")?, field("name")?);
    let session = format!("{}:", caller.session);
    let (mut args, target) = match split {
        None => (vec!["new-window"], session.as_str()),
        Some("h") => (vec!["split-window", "-h"], caller.pane.as_str()),
        Some("v") => (vec!["split-window", "-v"], caller.pane.as_str()),
        _ => return Err(Refusal::internal("invalid attachment placement")),
    };
    args.extend([
        "-d",
        "-P",
        "-F",
        "#{pane_id}|#{window_id}|#{pane_start_command}",
        "-t",
        target,
    ]);
    if split.is_none() {
        args.extend(["-n", name]);
    }
    // A Unix pane gets the server's environment, not the caller's: the state root reaches the view as it does the
    // bar (#103). Windows panes inherit it and get none.
    let env = super::spawn::pane_env(utf8(&ctx.state_root)?, crate::state::root::HOST);
    args.extend(env.iter().map(String::as_str));
    args.extend(["-c", directory, utf8(core.binary())?, "attach", address]);
    let reply = native(ctx, &args, false)?;
    let view = created_pane(&reply.stdout)
        .filter(|v| !v.command.is_empty())
        .ok_or_else(|| Refusal::internal("unproven new view"))?;
    *pane = Some(view.clone());
    decorate(ctx, caller, record, &view)?;
    let proof = format!("#{{pane_id}}|{}", launch(&view).launched());
    let read = native(
        ctx,
        &["display-message", "-p", "-t", &view.pane, &proof],
        false,
    )?;
    if read.stdout.trim_end_matches(['\r', '\n']) != format!("{}|1", view.pane) {
        return Err(Refusal::internal("unproven new view"));
    }
    Ok(())
}
fn launch(view: &Created) -> registry::Launch {
    registry::Launch {
        command: view.command.clone(),
        pid: None,
    }
}
fn bound(ctx: &Ctx, view: &Created, command: String) -> Res<()> {
    let args = registry::bound(&view.pane, launch(view).launched(), command);
    native(
        ctx,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
        false,
    )
    .map(|_| ())
}
pub fn clean(ctx: &Ctx, view: &Created) -> Res<bool> {
    if view.command.is_empty() || !super::pane_id(&view.pane) {
        return Ok(false);
    }
    let _ = bound(ctx, view, format!("kill-pane -t {}", view.pane));
    registry::pane_gone(ctx, &view.pane)
}
fn decorate(ctx: &Ctx, caller: &Caller, record: &Value, view: &Created) -> Res<()> {
    let field = |key| {
        record
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| Refusal::internal("invalid attachment tags"))
    };
    let context = format!("{}:{}", field("context")?, field("reason")?);
    let mut tags = vec![("@agent-id", field("id")?), ("@agent-name", field("name")?)];
    // A leader's view has no parent pane, so no parent tag (#103).
    if !caller.pane.is_empty() {
        tags.push(("@agent-parent", caller.pane.as_str()));
    }
    tags.push(("@agent-context", context.as_str()));
    if !tags.iter().all(|(_, v)| super::spawn::plain_token(v)) {
        return Err(Refusal::internal("invalid attachment tags"));
    }
    bound(
        ctx,
        view,
        tags.iter()
            .map(|(k, v)| format!("set-option -p -t {} {k} {v}", view.pane))
            .collect::<Vec<_>>()
            .join(" ; "),
    )?;
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = bound(
            ctx,
            view,
            format!("select-pane -T {} -t {}", field("name")?, view.pane),
        );
        bound(
            ctx,
            view,
            format!("set-option -p -t {} allow-set-title off", view.pane),
        )
    }));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agents::{core_fixture, fixture::World},
        hc,
    };
    #[test]
    fn attachment_uses_connected_binary_and_exact_proof() {
        let w = World::human("");
        let caller = w.caller();
        let bin = w.fakes.hc_install("herdr-core");
        core_fixture::answer(&w, &["--version"], "hc 0.1.21");
        let core = hc::connect(&w.ctx).unwrap();
        w.fakes.hc_install("hc");
        let command = "literal,#} attach t1";
        let launch = registry::Launch {
            command: command.into(),
            pid: None,
        };
        w.fakes
            .mux_rule("new-window", 0, &format!("%9|@5|{command}\n"), "");
        w.launches_hold();
        w.fakes
            .mux_rule("-t %9 #{pane_id}|#{@agent-id}", 0, "|\n", "");
        let r = serde_json::json!({"id":"aaaaaaaaaaaa","name":"worker","hcAddress":"t1","directory":w.ctx.cwd,"context":"brief","reason":"human"});
        let mut view = None;
        crate::agents::view::attach_core(&w.ctx, &core, &caller, &r, None, &mut view).unwrap();
        let view = view.unwrap();
        let calls = w.fakes.mux_calls();
        let create = calls.iter().find(|a| a[1] == "new-window").unwrap();
        assert_eq!(
            &create[create.len() - 3..],
            [utf8(&bin).unwrap(), "attach", "t1"]
        );
        assert!(calls.iter().any(|a| a
            == &vec![
                "-N",
                "display-message",
                "-p",
                "-t",
                "%9",
                &format!("#{{pane_id}}|{}", launch.launched())
            ]));
        assert!(crate::agents::view::clean_core(&w.ctx, &view).unwrap());
        assert!(w.fakes.mux_calls().iter().any(|a| a
            == &vec![
                "-N",
                "if-shell",
                "-F",
                "-t",
                "%9",
                &launch.launched(),
                "kill-pane -t %9"
            ]));
        w.fakes.clear_log();
        assert!(
            !clean(
                &w.ctx,
                &Created {
                    command: String::new(),
                    ..view
                }
            )
            .unwrap()
        );
        assert!(w.fakes.mux_calls().is_empty());
        w.fakes.hc_done().unwrap();
    }

    #[test]
    fn a_paneless_caller_attaches_a_window_without_a_parent_tag() {
        let w = World::human("");
        let mut caller = w.caller();
        caller.pane = String::new();
        core_fixture::online(&w);
        let core = hc::connect(&w.ctx).unwrap();
        w.fakes.mux_rule(
            "new-window",
            0,
            "%9|@5|hc attach t1
",
            "",
        );
        w.launches_hold();
        w.fakes.mux_rule(
            "-t %9 #{pane_id}|#{@agent-id}",
            0,
            "|
",
            "",
        );
        let r = serde_json::json!({"id":"aaaaaaaaaaaa","name":"leader","hcAddress":"t1","directory":w.ctx.cwd,"context":"brief","reason":"leader"});
        attach(&w.ctx, &core, &caller, &r, None, &mut None).unwrap();
        assert!(
            !w.fakes
                .mux_calls()
                .iter()
                .flatten()
                .any(|t| t.contains("@agent-parent"))
        );
        assert_eq!(
            attach(&w.ctx, &core, &caller, &r, Some("h"), &mut None)
                .unwrap_err()
                .message,
            "unexpected error (invalid attachment identity)"
        );
    }

    /// A Unix pane gets the server's environment, not the caller's, so the view is given the state root as the bar is
    /// (`-e MUX_AGENTS_STATE=<root>`, #103); a Windows pane inherits it and gets no `-e`.
    #[test]
    fn the_view_carries_the_state_root_into_a_unix_pane() {
        let w = World::human("");
        let caller = w.caller();
        core_fixture::online(&w);
        let core = hc::connect(&w.ctx).unwrap();
        w.fakes
            .mux_rule("new-window", 0, "%9|@5|hc attach t1\n", "");
        w.launches_hold();
        w.fakes
            .mux_rule("-t %9 #{pane_id}|#{@agent-id}", 0, "|\n", "");
        let r = serde_json::json!({"id":"aaaaaaaaaaaa","name":"worker","hcAddress":"t1","directory":w.ctx.cwd,"context":"brief","reason":"human"});
        attach(&w.ctx, &core, &caller, &r, None, &mut None).unwrap();
        let calls = w.fakes.mux_calls();
        let create = calls
            .iter()
            .find(|c| c.iter().any(|t| t == "new-window"))
            .unwrap();
        let at = create.iter().position(|t| t == "-c").unwrap();
        #[cfg(unix)]
        assert_eq!(
            &create[at - 2..at],
            [
                "-e".to_string(),
                format!("MUX_AGENTS_STATE={}", w.ctx.state_root.display())
            ],
            "{create:?}"
        );
        #[cfg(windows)]
        assert!(
            !create.iter().any(|t| t == "-e") && create[at - 2] == "-n",
            "{create:?}"
        );
    }
}
