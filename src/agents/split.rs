//! Where a split agent view goes, so the main chat keeps the most room: the first agent splits the caller in half,
//! the second and third stack evenly in that half, and a fourth, or any agent the room would leave too small, opens
//! as a tab instead. Existing panes are never moved; a stacked column is evened out by resizing its own agent views.
//! Anything that cannot be read or proven opens a tab, which touches no existing pane.

use crate::agents::registry::{self, Launch, Pane};
use crate::caller::Caller;
use crate::ctx::Ctx;
use crate::mux::native;
use crate::sidebar::{Sizing, bar_width};

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

/// What the caller's window looks like: the caller, its agent views (layout order), the window's width, the bar's
/// width when the window has a bar, and the width a bar opening after this split would take (0 when the session's
/// bar is closed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub caller: Geo,
    pub agents: Vec<Geo>,
    pub width: u32,
    pub bar: Option<u32>,
    pub bar_next: u32,
}

/// A split: `split-window <flag> -l 50% -t <target>`, then the agent views evened out when `even`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub target: String,
    pub flag: &'static str,
    pub even: bool,
}

/// The split for one more agent in `dir` (`h`: agents to the right of the caller, stacked; `v`: below it, side by
/// side), or none for a tab.
pub fn plan(dir: &str, w: &Window) -> Option<Plan> {
    let n = w.agents.len();
    if n >= MAX_SPLIT {
        return None;
    }
    // `h` stacks views top to bottom (each needs rows, the column needs width); `v` the other way round.
    let rows = match dir {
        "h" => true,
        "v" => false,
        _ => return None,
    };
    // The columns the caller has once a bar that is not open yet takes its share (it opens after this split).
    let caller_cols = match w.bar {
        Some(_) => w.caller.width,
        None if w.bar_next == 0 => w.caller.width,
        None => w.caller.width.saturating_sub(w.bar_next + 1),
    };
    let half = |size: u32| size.saturating_sub(1) / 2;
    if n == 0 {
        let ok = if rows {
            half(caller_cols) >= MIN_COLS && w.caller.height >= MIN_ROWS
        } else {
            half(w.caller.height) >= MIN_ROWS && caller_cols >= MIN_COLS
        };
        return ok.then(|| Plan {
            target: w.caller.pane.clone(),
            flag: if rows { "-h" } else { "-v" },
            even: false,
        });
    }
    let across = |g: &Geo| if rows { g.height } else { g.width };
    let along = |g: &Geo| if rows { g.width } else { g.height };
    let (min_across, min_along) = if rows {
        (MIN_ROWS, MIN_COLS)
    } else {
        (MIN_COLS, MIN_ROWS)
    };
    // The agent column (h) or row (v): split its largest view (a view dragged to a sliver cannot split), then
    // share the whole column evenly between n + 1 views.
    let target = w.agents.iter().max_by_key(|g| across(g))?;
    let total: u32 = w.agents.iter().map(across).sum::<u32>() + u32::try_from(n - 1).ok()?;
    let each = total.saturating_sub(u32::try_from(n).ok()?) / u32::try_from(n + 1).ok()?;
    if each < min_across || across(target) < 3 || along(target) < min_along {
        return None;
    }
    Some(Plan {
        target: target.pane.clone(),
        flag: if rows { "-v" } else { "-h" },
        even: n + 1 > 2,
    })
}

// The caller's window, one row per pane: its id, size, the window's width, the bar's width options and whether the
// pane is the bar (its label is only read to size the plan, never to act on that pane).
pub(crate) const GEOMETRY: &str = concat!(
    "#{pane_id}|#{pane_width}|#{pane_height}|#{window_width}|",
    "#{s/[|\n\r]/x/:@mux-agents-sidebar-min}|#{s/[|\n\r]/x/:@mux-agents-sidebar-percent}|#{@agent-role}"
);

/// The caller's window as `plan` needs it: its agent views are the panes of `views` that sit in this window, the
/// caller excluded. None when the window cannot be read or answers in another shape.
pub fn window(ctx: &Ctx, caller: &Caller, views: &[String]) -> Option<Window> {
    let read = native(
        ctx,
        &["list-panes", "-t", &caller.pane, "-F", GEOMETRY],
        true,
    )
    .ok()
    .filter(|r| r.code == 0)?;
    let mut rows = Vec::new();
    for line in read.stdout.lines() {
        let f: Vec<&str> = line.trim_end_matches('\r').splitn(7, '|').collect();
        let [pane, width, height, window, min, percent, role] = f[..] else {
            return None;
        };
        let n = |t: &str| t.parse::<u32>().ok();
        let sizing = Sizing::read(min, percent);
        rows.push((
            pane,
            n(width)?,
            n(height)?,
            n(window)?,
            sizing,
            role == "sidebar",
        ));
    }
    let geo = |r: &(&str, u32, u32, u32, Sizing, bool)| Geo {
        pane: r.0.into(),
        width: r.1,
        height: r.2,
    };
    let caller_row = rows.iter().find(|r| r.0 == caller.pane)?;
    let width = caller_row.3;
    let agents = rows
        .iter()
        .filter(|r| r.0 != caller.pane && views.iter().any(|v| v == r.0))
        .map(geo)
        .collect();
    // A bar the session closed does not open after the split.
    let closed = native(
        ctx,
        &[
            "show-options",
            "-qv",
            "-t",
            &caller.session,
            "@mux-agents-sidebar",
        ],
        true,
    )
    .is_ok_and(|r| r.stdout.trim_end_matches(['\r', '\n']) == "off");
    Some(Window {
        caller: geo(caller_row),
        agents,
        width,
        bar: rows.iter().find(|r| r.5).map(|r| r.1),
        bar_next: if closed {
            0
        } else {
            bar_width(width, caller_row.4)
        },
    })
}

/// The session's live member views, as the board proves them (pane and exact start command); none when the
/// members or the panes cannot be read, so the caller opens a tab.
pub fn views(ctx: &Ctx, caller: &Caller) -> Option<Vec<Pane>> {
    let rows = registry::rows(ctx, caller).ok()?;
    let locals = super::membership::locals(ctx, caller).ok()?;
    let key = super::core_identity::Key::of(caller);
    Some(
        locals
            .iter()
            .filter(|l| l.stored.committed)
            .filter_map(|l| l.identity.view(&l.stored.record, &rows, &key))
            .collect(),
    )
}

/// Whether `pane` is still the view `views` proved (its launch, alive), read right before it is split.
pub fn still(ctx: &Ctx, pane: &str, views: &[Pane]) -> bool {
    let Some(view) = views.iter().find(|v| v.pane == pane) else {
        return false;
    };
    let probe = format!("#{{pane_id}}|{}", view.launch.live());
    native(ctx, &["display-message", "-p", "-t", pane, &probe], true).is_ok_and(|r| {
        r.code == 0 && r.stdout.trim_end_matches(['\r', '\n']) == format!("{pane}|1")
    })
}

/// After a split, the agent views of the column shared evenly: every view but the last, in layout order, resized to
/// its share, each only while its pane is still that view's launch (`made` is the view just created and the command
/// it started). Best effort: no error or panic here reaches the attach.
pub fn even_out(ctx: &Ctx, caller: &Caller, dir: &str, views: &[Pane], made: (&str, &str)) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut launches: Vec<(String, Launch)> = (views.iter())
            .map(|v| (v.pane.clone(), v.launch.clone()))
            .collect();
        launches.push((
            made.0.into(),
            Launch {
                command: made.1.into(),
                pid: None,
            },
        ));
        let panes: Vec<String> = launches.iter().map(|(p, _)| p.clone()).collect();
        let Some(w) = window(ctx, caller, &panes) else {
            return;
        };
        let rows = dir == "h";
        let size = |g: &Geo| if rows { g.height } else { g.width };
        let n = u32::try_from(w.agents.len()).unwrap_or(0);
        if n < 2 {
            return;
        }
        // The views' own cells shared out: the first `extra` views get one more, so no two differ by more than one.
        let cells: u32 = w.agents.iter().map(size).sum();
        let (each, extra) = (cells / n, cells % n);
        let flag = if rows { "-y" } else { "-x" };
        for (at, g) in (0u32..).zip(&w.agents[..w.agents.len() - 1]) {
            let each = each + u32::from(at < extra);
            let Some((_, launch)) = launches.iter().find(|(p, _)| *p == g.pane) else {
                continue;
            };
            let pane = &g.pane;
            let resize = registry::bound(
                pane,
                launch.live(),
                format!("resize-pane -t {pane} {flag} {each}"),
            );
            let resize: Vec<&str> = resize.iter().map(String::as_str).collect();
            let _ = native(ctx, &resize, true);
        }
    }));
}

/// The main chat keeps its half: after a bar step, when `agent` (a view split to the right of the caller, `h`) is
/// wider than the caller (a bar that opened after the split took its columns from the caller), the caller is widened
/// to half of the two. A caller as wide or wider (a width the person dragged) is left alone, and a `v` split, which
/// shares height, never resizes. Best effort: nothing here fails the verb.
pub fn keep_main_half(ctx: &Ctx, caller: &Caller, agent: &str) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if !super::pane_id(&caller.pane) || !super::pane_id(agent) {
            return;
        }
        let Some(w) = window(ctx, caller, &[agent.to_string()]) else {
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
    }));
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

    fn split(target: &str, flag: &'static str, even: bool) -> Option<Plan> {
        Some(Plan {
            target: target.into(),
            flag,
            even,
        })
    }

    #[test]
    fn the_first_agent_halves_the_caller_and_main_keeps_the_rest() {
        let w = wide(Vec::new(), 199);
        assert_eq!(plan("h", &w), split("%1", "-h", false));
        assert_eq!(plan("v", &w), split("%1", "-v", false));
    }

    #[test]
    fn the_second_and_third_stack_evenly_in_the_agent_column() {
        let one = wide(vec![geo("%2", 99, 60)], 99);
        assert_eq!(plan("h", &one), split("%2", "-v", false));
        // Two views of 29 and 30 rows: the third splits the larger, then the column is evened out.
        let two = wide(vec![geo("%2", 99, 29), geo("%3", 99, 30)], 99);
        assert_eq!(plan("h", &two), split("%3", "-v", true));
        // A view the person dragged to 1 row: the split goes to the large one, never the one that cannot split.
        let dragged = wide(vec![geo("%2", 99, 58), geo("%3", 99, 1)], 99);
        assert_eq!(plan("h", &dragged), split("%2", "-v", true));
    }

    #[test]
    fn v_mirrors_with_views_side_by_side_under_main() {
        // Main on top (240x29), the agent row below it.
        let base = |agents: Vec<Geo>| Window {
            caller: geo("%1", 240, 29),
            agents,
            width: 240,
            bar: None,
            bar_next: 0,
        };
        assert_eq!(
            plan("v", &base(vec![geo("%2", 240, 30)])),
            split("%2", "-h", false)
        );
        assert_eq!(
            plan("v", &base(vec![geo("%2", 119, 30), geo("%3", 120, 30)])),
            split("%3", "-h", true)
        );
        // Three views of 60 columns need 182: a 150-column row is too narrow, a tab.
        assert_eq!(
            plan("v", &base(vec![geo("%2", 74, 30), geo("%3", 75, 30)])),
            None
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
        // The bar closed for the session: the whole 140 columns are the caller's, two 69-column halves.
        let closed = Window {
            caller: geo("%1", 140, 60),
            width: 140,
            bar_next: 0,
            ..narrow.clone()
        };
        assert_eq!(plan("h", &closed), split("%1", "-h", false));
        // A bar set wider (100 columns) in 200: 99 left, under two halves of 60.
        let wide_bar = Window {
            caller: geo("%1", 200, 60),
            width: 200,
            bar_next: 100,
            ..narrow.clone()
        };
        assert_eq!(plan("h", &wide_bar), None);
        // A short window: a second agent would get under 10 rows.
        let short = Window {
            caller: geo("%1", 99, 18),
            agents: vec![geo("%2", 99, 18)],
            width: 240,
            bar: Some(40),
            bar_next: 48,
        };
        assert_eq!(plan("h", &short), None);
        assert_eq!(plan("tab", &wide(Vec::new(), 199)), None);
    }

    #[test]
    fn the_window_read_counts_member_views_only_and_sizes_the_bar() {
        let w = crate::agents::fixture::World::human("");
        let caller = w.caller();
        w.fakes.mux_first(
            GEOMETRY,
            0,
            "%1|99|60|240|||\n%2|99|30|240|||\n%5|99|29|240|||\n%13|40|60|240|||sidebar\n",
            "",
        );
        let got = window(&w.ctx, &caller, &["%2".into(), "%9".into()]).unwrap();
        assert_eq!(got.caller, geo("%1", 99, 60));
        assert_eq!(got.agents, [geo("%2", 99, 30)], "%5 is no member view");
        assert_eq!((got.width, got.bar, got.bar_next), (240, Some(40), 48));
        // The width options as the caller's pane reads them; the bar closed for the session takes nothing.
        w.fakes
            .mux_first(GEOMETRY, 0, "%1|240|60|240|100|10|\n", "");
        assert_eq!(window(&w.ctx, &caller, &[]).unwrap().bar_next, 100);
        w.fakes
            .mux_first("show-options -qv -t $3 @mux-agents-sidebar", 0, "off\n", "");
        assert_eq!(window(&w.ctx, &caller, &[]).unwrap().bar_next, 0);
        // An answer in another shape reads as nothing: the spawn opens a tab.
        w.fakes.mux_first(GEOMETRY, 0, "%1|x|60|240|||\n", "");
        assert_eq!(window(&w.ctx, &caller, &[]), None);
    }

    #[test]
    fn unreadable_members_or_panes_read_as_no_views() {
        let w = crate::agents::fixture::World::human("");
        let caller = w.caller();
        w.fakes.mux_rule("list-panes -s", 1, "", "no server\n");
        assert_eq!(views(&w.ctx, &caller), None);
    }

    #[test]
    fn a_view_is_split_only_while_it_is_still_its_launch() {
        let w = crate::agents::fixture::World::human("");
        let view = Pane {
            pane: "%2".into(),
            window: "@1".into(),
            pid: "1".into(),
            id: "aaaaaaaaaaaa".into(),
            name: "a".into(),
            record: serde_json::Value::Null,
            launch: Launch {
                command: "hc attach t1".into(),
                pid: None,
            },
        };
        let probe = format!("#{{pane_id}}|{}", view.launch.live());
        w.fakes.mux_first(&probe, 0, "%2|1\n", "");
        assert!(still(&w.ctx, "%2", std::slice::from_ref(&view)));
        // Respawned with another command, or gone: no split there.
        w.fakes.mux_first(&probe, 0, "%2|0\n", "");
        assert!(!still(&w.ctx, "%2", std::slice::from_ref(&view)));
        w.fakes.mux_first(&probe, 1, "", "can't find pane\n");
        assert!(!still(&w.ctx, "%2", std::slice::from_ref(&view)));
        assert!(
            !still(&w.ctx, "%9", std::slice::from_ref(&view)),
            "no view proved there"
        );
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
            "%1|31|50|200|||\n%2|127|50|200|||\n%13|40|50|200|||sidebar\n",
            "",
        );
        keep_main_half(&w.ctx, &caller, "%2");
        assert_eq!(
            resizes(&w),
            [vec!["-N", "resize-pane", "-t", "%1", "-x", "79"]]
        );
        // A main the person widened stays; so does a `v` view below main (as wide, shorter).
        for answer in [
            "%1|120|50|200|||\n%2|38|50|200|||\n%13|40|50|200|||sidebar\n",
            "%1|189|39|240|||\n%2|189|40|240|||\n",
            "%1|1|39|240|||\n%2|189|20|240|||\n",
        ] {
            w.fakes.clear_log();
            w.fakes.mux_first(GEOMETRY, 0, answer, "");
            keep_main_half(&w.ctx, &caller, "%2");
            assert!(resizes(&w).is_empty(), "{answer}");
        }
    }

    #[test]
    fn even_out_resizes_every_view_but_the_last_and_only_its_own() {
        let w = crate::agents::fixture::World::human("");
        let caller = w.caller();
        // The column after the third split of the larger view: %2 14, %4 (new) 15, %3 1 of 32 rows.
        w.fakes.mux_first(
            GEOMETRY,
            0,
            "%1|99|32|200|||\n%2|100|14|200|||\n%4|100|15|200|||\n%3|100|1|200|||\n",
            "",
        );
        let view = |pane: &str| Pane {
            pane: pane.into(),
            window: "@1".into(),
            pid: "1".into(),
            id: "aaaaaaaaaaaa".into(),
            name: "a".into(),
            record: serde_json::Value::Null,
            launch: Launch {
                command: format!("hc attach {pane}"),
                pid: None,
            },
        };
        let views = [view("%2"), view("%3")];
        even_out(&w.ctx, &caller, "h", &views, ("%4", "hc attach new"));
        let resizes: Vec<String> = (w.fakes.mux_calls().into_iter())
            .filter(|c| c[1] == "if-shell")
            .map(|c| c[c.len() - 1].clone())
            .collect();
        assert_eq!(
            resizes,
            ["resize-pane -t %2 -y 10", "resize-pane -t %4 -y 10"]
        );
        // A panic in a multiplexer call stays here.
        w.fakes.mux_script("if-shell", |_| panic!("mux"));
        even_out(&w.ctx, &caller, "h", &views, ("%4", "hc attach new"));
    }
}
