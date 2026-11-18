//! Where a split agent view goes, so the main chat keeps the most room: the first agent splits the caller in half,
//! the second and third stack evenly in that half, and a fourth, or any agent the room would leave too small, opens
//! as a tab instead. Existing panes are never moved; a stacked column is evened out by resizing its own agent views.

use crate::agents::registry::{self, Pane};
use crate::caller::Caller;
use crate::ctx::Ctx;
use crate::mux::native;

/// The narrowest an agent view or the main chat is split to, in columns: below it a tab reads better.
pub const MIN_COLS: u32 = 60;
/// The shortest an agent view is split to, in rows.
pub const MIN_ROWS: u32 = 10;
/// Agent views split beside the caller before the next one opens as a tab.
pub const MAX_SPLIT: usize = 3;

/// One pane's size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Geo {
    pub pane: String,
    pub width: u32,
    pub height: u32,
}

/// What the caller's window looks like: the caller, its agent views (layout order), the window's width, and the bar's
/// width when the window has a bar (else the width a bar opening next will take).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub caller: Geo,
    pub agents: Vec<Geo>,
    pub width: u32,
    pub bar: Option<u32>,
    pub bar_next: u32,
}

/// A split: `split-window <flag> -l 50% -t <target>`, then `even` resizes, `(pane, flag, size)`, best effort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub target: String,
    pub flag: &'static str,
    pub even: Vec<(String, &'static str, u32)>,
}

/// The split for one more agent in `dir` (`h`: agents to the right of the caller, stacked; `v`: below it, side by
/// side), or none for a tab.
pub fn plan(dir: &str, w: &Window) -> Option<Plan> {
    let n = w.agents.len();
    if n >= MAX_SPLIT {
        return None;
    }
    // The columns the caller has once a bar that is not open yet takes its share (it opens after this split).
    let caller_cols = match w.bar {
        Some(_) => w.caller.width,
        None => w.caller.width.saturating_sub(w.bar_next + 1),
    };
    let half = |size: u32| size.saturating_sub(1) / 2;
    // `h` stacks views top to bottom (each needs rows, the column needs width); `v` the other way round.
    let rows = match dir {
        "h" => true,
        "v" => false,
        _ => return None,
    };
    let across = |g: &Geo| if rows { g.height } else { g.width };
    let along = |g: &Geo| if rows { g.width } else { g.height };
    let (min_across, min_along) = if rows {
        (MIN_ROWS, MIN_COLS)
    } else {
        (MIN_COLS, MIN_ROWS)
    };
    if n == 0 {
        let ok = match dir {
            "h" => half(caller_cols) >= MIN_COLS && w.caller.height >= MIN_ROWS,
            _ => half(w.caller.height) >= MIN_ROWS && caller_cols >= MIN_COLS,
        };
        let flag = if dir == "h" { "-h" } else { "-v" };
        return ok.then(|| Plan {
            target: w.caller.pane.clone(),
            flag,
            even: Vec::new(),
        });
    }
    // The agent column (h) or row (v): split its last view, then share it evenly between n + 1 views.
    let flag = if dir == "h" { "-v" } else { "-h" };
    let last = &w.agents[n - 1];
    let total: u32 = w.agents.iter().map(across).sum::<u32>() + u32::try_from(n - 1).ok()?;
    let each = total.saturating_sub(u32::try_from(n).ok()?) / u32::try_from(n + 1).ok()?;
    if each < min_across || along(last) < min_along {
        return None;
    }
    let resize = if dir == "h" { "-y" } else { "-x" };
    let even = if n + 1 > 2 {
        w.agents
            .iter()
            .map(|g| (g.pane.clone(), resize, each))
            .collect()
    } else {
        Vec::new()
    };
    Some(Plan {
        target: last.pane.clone(),
        flag,
        even,
    })
}

// The caller's window, one row per pane: its id, size, the window's width and whether it is the bar (its label is
// only read to size the plan, never to act on that pane).
pub(crate) const GEOMETRY: &str =
    "#{pane_id}|#{pane_width}|#{pane_height}|#{window_width}|#{@agent-role}";

/// The caller's window as `plan` needs it: its agent views are the session's member views (proven by pane and exact
/// start command, `views`) that sit in this window, the caller excluded. None when the window cannot be read.
pub fn window(ctx: &Ctx, caller: &Caller, views: &[Pane]) -> Option<Window> {
    let read = native(
        ctx,
        &["list-panes", "-t", &caller.pane, "-F", GEOMETRY],
        true,
    )
    .ok()
    .filter(|r| r.code == 0)?;
    let mut rows = Vec::new();
    for line in read.stdout.lines() {
        let f: Vec<&str> = line.trim_end_matches('\r').splitn(5, '|').collect();
        let [pane, width, height, window, role] = f[..] else {
            return None;
        };
        let n = |t: &str| t.parse::<u32>().ok();
        rows.push((pane, n(width)?, n(height)?, n(window)?, role == "sidebar"));
    }
    let geo = |&(pane, width, height, ..): &(&str, u32, u32, u32, bool)| Geo {
        pane: pane.into(),
        width,
        height,
    };
    let caller_row = rows.iter().find(|r| r.0 == caller.pane)?;
    let width = caller_row.3;
    let agents = rows
        .iter()
        .filter(|r| r.0 != caller.pane && views.iter().any(|v| v.pane == r.0))
        .map(geo)
        .collect();
    Some(Window {
        caller: geo(caller_row),
        agents,
        width,
        bar: rows.iter().find(|r| r.4).map(|r| r.1),
        bar_next: crate::sidebar::bar_width(width, crate::sidebar::Sizing::default()),
    })
}

/// The session's live member views, as the board proves them; none when they cannot be read.
pub fn views(ctx: &Ctx, caller: &Caller) -> Vec<Pane> {
    let Ok(rows) = registry::rows(ctx, caller) else {
        return Vec::new();
    };
    let Ok(locals) = super::membership::locals(ctx, caller) else {
        return Vec::new();
    };
    let key = super::core_identity::Key::of(caller);
    locals
        .iter()
        .filter(|l| l.stored.committed)
        .filter_map(|l| l.identity.view(&l.stored.record, &rows, &key))
        .collect()
}

/// After a split, the stacked views shared evenly, each resize only while its pane is still that view's launch.
pub fn even_out(ctx: &Ctx, plan: &Plan, views: &[Pane]) {
    for (pane, flag, size) in &plan.even {
        let Some(view) = views.iter().find(|v| &v.pane == pane) else {
            continue;
        };
        let command = registry::bound(
            pane,
            view.launch.live(),
            format!("resize-pane -t {pane} {flag} {size}"),
        );
        let command: Vec<&str> = command.iter().map(String::as_str).collect();
        let _ = native(ctx, &command, true);
    }
}

/// The main chat keeps its half: after a spawn's bar step, when `agent` (the view just split to the right of the
/// caller, `h`) is wider than the caller (a bar that opened after the split took its columns from the caller), the
/// caller is widened to half of the two. A caller as wide or wider (a width the person dragged) is left alone, and a
/// `v` split, which shares height, never resizes. Best effort: nothing here fails the spawn.
pub fn keep_main_half(ctx: &Ctx, caller: &Caller, agent: &str) {
    if !super::pane_id(&caller.pane) || !super::pane_id(agent) {
        return;
    }
    // The view counts as an agent of this window when it is the one just made.
    let made = Pane {
        pane: agent.into(),
        window: String::new(),
        pid: String::new(),
        id: String::new(),
        name: String::new(),
        record: serde_json::Value::Null,
        launch: registry::Launch {
            command: String::new(),
            pid: None,
        },
    };
    let Some(w) = window(ctx, caller, std::slice::from_ref(&made)) else {
        return;
    };
    let Some(view) = w.agents.iter().find(|g| g.pane == agent) else {
        return;
    };
    // Side by side only: a view below or above the caller (`v`) is as wide as it.
    if view.height == w.caller.height && view.width > w.caller.width + 1 {
        let half = ((w.caller.width + view.width) / 2).to_string();
        let _ = native(ctx, &["resize-pane", "-t", &caller.pane, "-x", &half], true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geo(pane: &str, width: u32, height: u32) -> Geo {
        Geo {
            pane: pane.into(),
            width,
            height,
        }
    }

    // A 240x60 window, main alone on the left, the bar already open (40 columns).
    fn wide(agents: Vec<Geo>, caller_width: u32) -> Window {
        Window {
            caller: geo("%1", caller_width, 60),
            agents,
            width: 240,
            bar: Some(40),
            bar_next: 48,
        }
    }

    #[test]
    fn the_first_agent_halves_the_caller_and_main_keeps_the_rest() {
        let w = wide(Vec::new(), 199);
        assert_eq!(
            plan("h", &w),
            Some(Plan {
                target: "%1".into(),
                flag: "-h",
                even: Vec::new()
            })
        );
        assert_eq!(plan("v", &w).map(|p| p.flag), Some("-v"));
    }

    #[test]
    fn the_second_and_third_stack_evenly_in_the_agent_column() {
        let one = wide(vec![geo("%2", 99, 60)], 99);
        assert_eq!(
            plan("h", &one),
            Some(Plan {
                target: "%2".into(),
                flag: "-v",
                even: Vec::new()
            })
        );
        // Two views of 29 and 30 rows: the third splits the last, then all three get (60 - 2) / 3 rows.
        let two = wide(vec![geo("%2", 99, 29), geo("%3", 99, 30)], 99);
        assert_eq!(
            plan("h", &two),
            Some(Plan {
                target: "%3".into(),
                flag: "-v",
                even: vec![("%2".into(), "-y", 19), ("%3".into(), "-y", 19)]
            })
        );
    }

    #[test]
    fn a_fourth_agent_opens_as_a_tab() {
        let three = wide(
            vec![geo("%2", 99, 19), geo("%3", 99, 19), geo("%4", 99, 20)],
            99,
        );
        assert_eq!(plan("h", &three), None);
        assert_eq!(plan("v", &three), None);
    }

    #[test]
    fn too_little_room_opens_a_tab() {
        // 120 columns, no bar yet (it will take 41): half of the 79 left is under 60.
        let narrow = Window {
            caller: geo("%1", 120, 60),
            agents: Vec::new(),
            width: 120,
            bar: None,
            bar_next: 40,
        };
        assert_eq!(plan("h", &narrow), None);
        // The same window with room once the bar is counted: 200 - 41 = 159, half 79.
        let room = Window {
            caller: geo("%1", 200, 60),
            width: 200,
            ..narrow.clone()
        };
        assert_eq!(plan("h", &room).map(|p| p.target), Some("%1".into()));
        // A short window: a second agent would get under 10 rows.
        let short = Window {
            caller: geo("%1", 99, 18),
            agents: vec![geo("%2", 99, 18)],
            width: 240,
            bar: Some(40),
            bar_next: 48,
        };
        assert_eq!(plan("h", &short), None);
        // `v`: the agent row needs 60 columns a view.
        let v = Window {
            caller: geo("%1", 110, 30),
            agents: vec![geo("%2", 110, 29)],
            width: 150,
            bar: Some(40),
            bar_next: 40,
        };
        assert_eq!(plan("v", &v), None);
        assert_eq!(plan("tab", &wide(Vec::new(), 199)), None);
    }

    #[test]
    fn the_window_read_counts_member_views_only_and_finds_the_bar() {
        let w = crate::agents::fixture::World::human("");
        let caller = w.caller();
        w.fakes.mux_first(
            GEOMETRY,
            0,
            "%1|99|60|240|\n%2|99|30|240|\n%5|99|29|240|\n%13|40|60|240|sidebar\n",
            "",
        );
        let view = |pane: &str| Pane {
            pane: pane.into(),
            window: "@1".into(),
            pid: "1".into(),
            id: "aaaaaaaaaaaa".into(),
            name: "a".into(),
            record: serde_json::Value::Null,
            launch: registry::Launch {
                command: "hc attach t1".into(),
                pid: None,
            },
        };
        let got = window(&w.ctx, &caller, &[view("%2"), view("%9")]).unwrap();
        assert_eq!(got.caller, geo("%1", 99, 60));
        assert_eq!(got.agents, [geo("%2", 99, 30)], "%5 is no member view");
        assert_eq!((got.width, got.bar), (240, Some(40)));
        // An answer in another shape reads as nothing: the spawn opens a tab.
        w.fakes.mux_first(GEOMETRY, 0, "%1|x|60|240|\n", "");
        assert_eq!(window(&w.ctx, &caller, &[]), None);
    }

    #[test]
    fn main_is_widened_to_half_only_when_narrower() {
        let w = crate::agents::fixture::World::human("");
        let caller = w.caller();
        let resizes = |w: &crate::agents::fixture::World| -> Vec<Vec<String>> {
            (w.fakes.mux_calls().into_iter())
                .filter(|c| c[1] == "resize-pane")
                .collect()
        };
        // rmux after the bar opened: main 31, the agent beside it 127, bar 40 in 200 columns.
        w.fakes.mux_first(
            GEOMETRY,
            0,
            "%1|31|50|200|\n%2|127|50|200|\n%13|40|50|200|sidebar\n",
            "",
        );
        keep_main_half(&w.ctx, &caller, "%2");
        assert_eq!(
            resizes(&w),
            [vec!["-N", "resize-pane", "-t", "%1", "-x", "79"]]
        );
        // A main the person widened stays; so does a `v` view below main (as wide, shorter).
        for answer in [
            "%1|120|50|200|\n%2|38|50|200|\n%13|40|50|200|sidebar\n",
            "%1|189|39|240|\n%2|189|40|240|\n",
            "%1|1|39|240|\n%2|189|20|240|\n",
        ] {
            w.fakes.clear_log();
            w.fakes.mux_first(GEOMETRY, 0, answer, "");
            keep_main_half(&w.ctx, &caller, "%2");
            assert!(resizes(&w).is_empty(), "{answer}");
        }
    }
}
