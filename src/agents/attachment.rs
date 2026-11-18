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
    address: &str,
    split: Option<&str>,
    pane: &mut Option<Created>,
) -> Res<()> {
    let field = |key| {
        record
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| Refusal::internal("invalid attachment identity"))
    };
    // Call 18a: the address is the selected identity's (a held one's is its pending generation's), never the record's.
    if address.is_empty()
        || !super::agent_id(field("id")?)
        || !super::agent_name(field("name")?)
        // A paneless caller (`up`'s leader) may only open a window: a split needs a pane to split.
        || !(super::pane_id(&caller.pane) || (caller.pane.is_empty() && split.is_none()))
    {
        return Err(Refusal::internal("invalid attachment identity"));
    }
    let (directory, name) = (field("directory")?, field("name")?);
    if !matches!(split, None | Some("h" | "v")) {
        return Err(Refusal::internal("invalid attachment placement"));
    }
    // A split goes where the main chat keeps the most room (`split::plan`); no room, or a window that cannot be read,
    // opens a tab.
    let views = split.and_then(|_| super::split::views(ctx, caller));
    let mut plan = split.zip(views.as_ref()).and_then(|(dir, views)| {
        let panes: Vec<String> = views.iter().map(|v| v.pane.clone()).collect();
        let window = super::split::window(ctx, caller, &panes)?;
        let plan = super::split::plan(dir, &window)?;
        // An agent view is split only while it is still that view's launch; the caller's own pane is the caller.
        (plan.target == caller.pane || super::split::still(ctx, &plan.target, views))
            .then_some(plan)
    });
    // A Unix pane gets the server's environment, not the caller's: the state root reaches the view as it does the
    // bar. Windows panes inherit it and get none.
    let env = super::spawn::pane_env(utf8(&ctx.state_root)?, crate::state::root::HOST);
    let program = utf8(core.binary())?;
    let session = format!("{}:", caller.session);
    let create = |plan: Option<&super::split::Plan>| -> Res<Created> {
        let (mut args, target) = match plan {
            None => (vec!["new-window"], session.as_str()),
            Some(p) => (vec!["split-window", p.flag], p.target.as_str()),
        };
        args.extend([
            "-d",
            "-P",
            "-F",
            "#{pane_id}|#{window_id}|#{pane_start_command}",
            "-t",
            target,
        ]);
        if plan.is_none() {
            args.extend(["-n", name]);
        }
        let tail: Vec<&str> = (env.iter().map(String::as_str))
            .chain(["-c", directory, program, "attach", address])
            .collect();
        let sized: Vec<&str> = (args.iter().copied())
            .chain(plan.map(|_| ["-l", "50%"]).into_iter().flatten())
            .chain(tail.iter().copied())
            .collect();
        // A multiplexer that refuses the size splits unsized, as the bar does.
        let reply = match native(ctx, &sized, true)? {
            r if r.code == 0 => r,
            r if plan.is_some() && crate::sidebar::size_refusal(&r) => {
                let plain: Vec<&str> = args.iter().copied().chain(tail.iter().copied()).collect();
                native(ctx, &plain, false)?
            }
            _ => return crate::mux::call_failed(ctx),
        };
        created_pane(&reply.stdout)
            .filter(|v| !v.command.is_empty())
            .ok_or_else(|| Refusal::internal("unproven new view"))
    };
    let mut view = create(plan.as_ref())?;
    // Ours from here on: any failure below closes it (the caller's rollback reads `pane`).
    *pane = Some(view.clone());
    // The split cannot be guarded where it runs (an `if-shell` drops the new pane's id), so an agent view split beside
    // is proven again after it: a pane replaced in between gets no view beside it; the new view, proven by its own
    // command, is closed and opened as a tab instead. A view that cannot be shown closed stays where it is.
    if let (Some(p), Some(views)) = (&plan, &views)
        && p.target != caller.pane
        && !super::split::still(ctx, &p.target, views)
        && clean(ctx, &view)?
    {
        *pane = None;
        view = create(None)?;
        *pane = Some(view.clone());
        plan = None;
    }
    if let (Some(plan), Some(views), Some(dir)) = (&plan, &views, split)
        && plan.even
    {
        super::split::even_out(ctx, caller, dir, views, (&view.pane, &view.command));
    }
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
    // A leader's view has no parent pane, so no parent tag.
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
        crate::agents::view::attach_core(&w.ctx, &core, &caller, &r, "t1", None, &mut view)
            .unwrap();
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
        attach(&w.ctx, &core, &caller, &r, "t1", None, &mut None).unwrap();
        assert!(
            !w.fakes
                .mux_calls()
                .iter()
                .flatten()
                .any(|t| t.contains("@agent-parent"))
        );
        assert_eq!(
            attach(&w.ctx, &core, &caller, &r, "t1", Some("h"), &mut None)
                .unwrap_err()
                .message,
            "unexpected error (invalid attachment identity)"
        );
    }

    /// A second agent splits the first agent's view; when that pane is replaced between the plan and the split, the
    /// new view is not left beside it: it is closed and opened as a tab.
    #[test]
    fn a_view_replaced_during_the_split_gets_a_tab_instead() {
        // (the target replaced after the split, the new view still there after its kill, the view kept)
        for (replaced, stuck, kept) in [
            (false, false, "%12"),
            (true, false, "%13"),
            (true, true, "%12"),
        ] {
            let case = format!("{replaced} {stuck}");
            let w = World::human("");
            let caller = w.caller();
            let first = core_fixture::member(&w, "bbbbbbbbbbbb", serde_json::json!({"pane":"%9"}));
            let mut row = crate::agents::fixture::row_of("%9|@1|300||", &[]);
            row.command = first["startCommand"].as_str().unwrap().into();
            w.pane_rows(&[row]);
            core_fixture::online(&w);
            let core = hc::connect(&w.ctx).unwrap();
            w.fakes.mux_first(
                crate::agents::split::GEOMETRY,
                0,
                "%1|119|60|240|||\n%9|120|60|240|||\n",
                "",
            );
            // `still` answers 1 for the plan, then (replaced) 0 right after the split.
            let probe = "#{pane_id}|#{&&:#{==:#{pane_dead},0},#{==:#{pane_start_command},hc attach tbbbbbbbbbbbb}}";
            let reads = std::cell::Cell::new(0);
            w.fakes.mux_script(probe, move |_| {
                reads.set(reads.get() + 1);
                let ok = !(replaced && reads.get() > 1);
                Ok(crate::mux::Output {
                    code: 0,
                    stdout: format!("%9|{}\n", u8::from(ok)),
                    stderr: String::new(),
                })
            });
            w.fakes
                .mux_rule("split-window", 0, "%12|@1|hc attach t1\n", "");
            w.fakes
                .mux_rule("new-window", 0, "%13|@6|hc attach t1\n", "");
            w.launches_hold();
            // The gone probe after the kill: %12 still answers when its kill did not take.
            w.fakes.mux_script("#{pane_id}|#{@agent-id}", move |argv| {
                let there = stuck && argv.iter().any(|a| a == "%12");
                Ok(crate::mux::Output {
                    code: 0,
                    stdout: if there { "%12|\n" } else { "|\n" }.into(),
                    stderr: String::new(),
                })
            });
            let r = serde_json::json!({"id":"aaaaaaaaaaaa","name":"worker","hcAddress":"t1","directory":w.ctx.cwd,"context":"brief","reason":"human"});
            let mut view = None;
            attach(&w.ctx, &core, &caller, &r, "t1", Some("h"), &mut view).unwrap();
            let calls = w.fakes.mux_calls();
            let split = calls
                .iter()
                .find(|c| c[1] == "split-window")
                .expect("a split");
            assert_eq!(
                split[split.iter().position(|a| a == "-t").unwrap() + 1],
                "%9"
            );
            let killed = calls
                .iter()
                .any(|c| c.last().is_some_and(|a| a == "kill-pane -t %12"));
            assert_eq!(killed, replaced, "{case}");
            assert_eq!(view.unwrap().pane, kept, "{case}");
            let tabs = calls.iter().filter(|c| c[1] == "new-window").count();
            assert_eq!(
                tabs,
                usize::from(kept == "%13"),
                "{case}: one view, never two"
            );
        }
    }

    /// A failure after the split leaves the split in `pane`, so the caller's rollback closes it.
    #[test]
    fn the_split_is_owned_before_anything_after_it_can_fail() {
        let w = World::human("");
        let caller = w.caller();
        let first = core_fixture::member(&w, "bbbbbbbbbbbb", serde_json::json!({"pane":"%9"}));
        let mut row = crate::agents::fixture::row_of("%9|@1|300||", &[]);
        row.command = first["startCommand"].as_str().unwrap().into();
        w.pane_rows(&[row]);
        core_fixture::online(&w);
        let core = hc::connect(&w.ctx).unwrap();
        w.fakes.mux_first(
            crate::agents::split::GEOMETRY,
            0,
            "%1|119|60|240|||\n%9|120|60|240|||\n",
            "",
        );
        // The target proves for the plan, then its re-check after the split fails as a call (the server went away).
        let probe = "#{pane_id}|#{&&:#{==:#{pane_dead},0},#{==:#{pane_start_command},hc attach tbbbbbbbbbbbb}}";
        let reads = std::cell::Cell::new(0);
        w.fakes.mux_script(probe, move |_| {
            reads.set(reads.get() + 1);
            Ok(crate::mux::Output {
                code: 0,
                stdout: if reads.get() == 1 { "%9|1\n" } else { "%9|0\n" }.into(),
                stderr: String::new(),
            })
        });
        w.fakes
            .mux_rule("split-window", 0, "%12|@1|hc attach t1\n", "");
        w.fakes.mux_rule("kill-pane", 1, "", "no server\n");
        w.fakes
            .mux_rule("#{pane_id}|#{@agent-id}", 1, "", "no server\n");
        let r = serde_json::json!({"id":"aaaaaaaaaaaa","name":"worker","hcAddress":"t1","directory":w.ctx.cwd,"context":"brief","reason":"human"});
        let mut view = None;
        assert!(attach(&w.ctx, &core, &caller, &r, "t1", Some("h"), &mut view).is_err());
        assert_eq!(view.map(|v| v.pane).as_deref(), Some("%12"));
    }

    /// A Unix pane gets the server's environment, not the caller's, so the view is given the state root as the bar is
    /// (`-e MUX_AGENTS_STATE=<root>`); a Windows pane inherits it and gets no `-e`.
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
        attach(&w.ctx, &core, &caller, &r, "t1", None, &mut None).unwrap();
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
