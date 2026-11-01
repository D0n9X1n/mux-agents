//! `placement [tab|h|v]`: where `spawn` opens an agent that names no place, kept as the session option
//! @mux-agents-placement on the caller's multiplexer session, so it goes with the session and no file holds it.

use serde_json::{Map, Value};

use super::{text, usage};
use crate::caller::{Caller, resolve};
use crate::cli::Request;
use crate::ctx::Ctx;
use crate::error::{Res, refuse};
use crate::mux::native;

const OPTION: &str = "@mux-agents-placement";
pub(crate) const DEFAULT: &str = "tab";

/// `tab`, `h` or `v` exactly: the values of `-Split` plus a new window.
pub(crate) fn known(value: &str) -> Option<&'static str> {
    ["tab", "h", "v"].into_iter().find(|known| *known == value)
}

/// The session's stored placement: none when unset or not one of the known values. A call that fails, or answers
/// nonzero, is the multiplexer's refusal, left to the caller to decide on.
pub(crate) fn read_placement(ctx: &Ctx, caller: &Caller) -> Res<Option<&'static str>> {
    let read = native(
        ctx,
        &["show-options", "-qv", "-t", &caller.session, OPTION],
        false,
    )?;
    Ok(known(read.stdout.trim_end_matches(['\r', '\n'])))
}

/// `placement` reads the session's placement (`tab` by default); `placement <value>` sets it on the caller's
/// multiplexer session. Refused from a tagged pane: only the top-level chat chooses.
pub fn placement(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let value = match request.positional.first() {
        None => None,
        Some(value) => Some(known(value).ok_or_else(|| usage("placement"))?),
    };
    let caller = resolve(ctx)?;
    if !caller.agent_tag.is_empty() {
        return refuse(2, "tagged", "an agent pane cannot choose where agents open");
    }
    let (placement, source) = match value {
        Some(value) => {
            native(
                ctx,
                &["set-option", "-t", &caller.session, OPTION, value],
                false,
            )?;
            (value, "session")
        }
        None => match read_placement(ctx, &caller)? {
            Some(stored) => (stored, "session"),
            None => (DEFAULT, "default"),
        },
    };
    let mut reply = Map::new();
    reply.insert("placement".into(), placement.into());
    reply.insert("source".into(), source.into());
    Ok(reply)
}

/// `placement <value> (<source>)`.
pub fn placement_text(reply: &Map<String, Value>, _color: bool) -> String {
    format!(
        "placement {} ({})",
        text(reply.get("placement")),
        text(reply.get("source"))
    )
}

#[cfg(test)]
mod tests {
    use crate::agents::fixture::{ID, World};
    use crate::proc::Signals;

    const READ: &str = "-N show-options -qv -t $3 @mux-agents-placement";

    // One call through the entry, as the dispatcher runs it: (exit code, reply text).
    fn call(w: &World, tokens: &[&str]) -> (u8, String) {
        let args: Vec<String> = tokens.iter().map(|t| t.to_string()).collect();
        crate::cli::run(&args, || Ok(&w.ctx))
    }

    // Every multiplexer call but the caller probe, as text.
    fn calls(w: &World) -> Vec<String> {
        w.fakes
            .mux_calls()
            .iter()
            .filter(|c| c[1] != "display-message")
            .map(|c| c.join(" "))
            .collect()
    }

    #[test]
    fn getter_default_and_session_json_and_text() {
        // Unset (empty) and unknown values read as the default.
        for stored in ["", "\n", "tabs\n", "H\n", "h v\n"] {
            let w = World::human("");
            w.fakes.mux_rule("show-options", 0, stored, "");
            assert_eq!(
                call(&w, &["placement"]),
                (
                    0,
                    r#"{"ok":true,"verb":"placement","placement":"tab","source":"default"}"#.into()
                ),
                "{stored:?}"
            );
            assert_eq!(calls(&w), [READ], "{stored:?}");
            assert_eq!(
                call(&w, &["placement", "-Text"]),
                (0, "placement tab (default)".into())
            );
        }
        for stored in ["tab", "h", "v"] {
            let w = World::human("");
            w.fakes
                .mux_rule("show-options", 0, &format!("{stored}\r\n"), "");
            assert_eq!(
                call(&w, &["placement"]),
                (
                    0,
                    format!(
                        r#"{{"ok":true,"verb":"placement","placement":"{stored}","source":"session"}}"#
                    )
                )
            );
            assert_eq!(
                call(&w, &["placement", "-text"]),
                (0, format!("placement {stored} (session)"))
            );
        }
        // An unreadable option is a failed call, not the default.
        let w = World::human("");
        w.fakes.mux_rule("show-options", 1, "", "no server");
        assert_eq!(
            call(&w, &["placement"]),
            (
                1,
                r#"{"ok":false,"verb":"placement","code":1,"check":"rmux","message":"rmux call failed"}"#
                    .into()
            )
        );
    }

    #[test]
    fn setter_sets_the_session_option() {
        for value in ["tab", "h", "v"] {
            let w = World::human("");
            assert_eq!(
                call(&w, &["placement", value]),
                (
                    0,
                    format!(
                        r#"{{"ok":true,"verb":"placement","placement":"{value}","source":"session"}}"#
                    )
                )
            );
            assert_eq!(
                calls(&w),
                [format!("-N set-option -t $3 @mux-agents-placement {value}")]
            );
            w.fakes.clear_log();
            assert_eq!(
                call(&w, &["PLACEMENT", value, "-Text"]),
                (0, format!("placement {value} (session)"))
            );
            // No file: the state folder is never made.
            assert!(!w.ctx.state_root.exists(), "{value}");
        }
        let w = World::human("");
        w.fakes.mux_rule("set-option", 1, "", "no session");
        assert_eq!(call(&w, &["placement", "h"]).0, 1);
    }

    #[test]
    fn bad_value_or_two_values_usage_without_mux_call() {
        let usage = r#"{"ok":false,"verb":"placement","code":2,"check":"usage","message":"usage: mux-agent placement [tab|h|v] [-Text]"}"#;
        for value in ["H", "Tab", "", "split", "h;kill-server", "%1"] {
            let w = World::human("");
            assert_eq!(
                call(&w, &["placement", value]),
                (2, usage.into()),
                "{value:?}"
            );
            assert!(w.fakes.mux_calls().is_empty(), "{value:?}");
        }
        let w = World::human("");
        let (code, out) = call(&w, &["placement", "h", "v"]);
        assert_eq!(code, 2, "{out}");
        assert!(out.contains(r#""check":"usage""#), "{out}");
        assert!(w.fakes.mux_calls().is_empty());
    }

    #[test]
    fn tagged_getter_and_setter_refuse() {
        for args in [&["placement"][..], &["placement", "h"]] {
            for tag in [ID, "x"] {
                let w = World::human(tag);
                assert_eq!(
                    call(&w, args),
                    (
                        2,
                        r#"{"ok":false,"verb":"placement","code":2,"check":"tagged","message":"an agent pane cannot choose where agents open"}"#
                            .into()
                    ),
                    "{args:?} {tag}"
                );
                assert!(calls(&w).is_empty(), "{args:?} {tag}");
            }
        }
    }

    #[test]
    fn sandboxed_refuses_before_any_mux_call() {
        for args in [&["placement"][..], &["placement", "v"]] {
            let w = World::human("");
            w.fakes.signals(Signals::SandboxCheck(1));
            assert_eq!(call(&w, args).0, 2, "{args:?}");
            assert!(w.fakes.mux_calls().is_empty(), "{args:?}");
            assert!(!w.ctx.state_root.exists(), "{args:?}");
        }
    }
}
