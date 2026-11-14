//! A team's spawn budget (D5): a token bucket in `teams/<sha16(session)>.budget.json`, spent under the team lock.
use crate::caller::Caller;
use crate::ctx::Ctx;
use crate::error::{Refusal, Res, refuse};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    pub capacity: u64,
    pub refill: u64,
}
pub const DEFAULT: Rate = Rate {
    capacity: 20,
    refill: 180,
};
pub fn path(server_dir: &Path, session: &str) -> PathBuf {
    server_dir.join("teams").join(format!(
        "{}.budget.json",
        &crate::util::sha256_hex(session)[..16]
    ))
}
/// The session options `@mux-agents-spawn-budget` and `@mux-agents-spawn-refill` (seconds), each 1..=86400; any other
/// value, or one that cannot be read, is the default.
pub fn rate(ctx: &Ctx, caller: &Caller) -> Rate {
    let read = |option: &str, default: u64| {
        crate::mux::native(
            ctx,
            &["show-options", "-qv", "-t", &caller.session, option],
            false,
        )
        .ok()
        .and_then(|o| o.stdout.trim_end_matches(['\r', '\n']).parse::<u64>().ok())
        .filter(|n| (1..=86_400).contains(n))
        .unwrap_or(default)
    };
    Rate {
        capacity: read("@mux-agents-spawn-budget", DEFAULT.capacity),
        refill: read("@mux-agents-spawn-refill", DEFAULT.refill),
    }
}
/// Takes one token at `now` (s), on disk before it returns; the caller holds the team lock. Empty refuses 2 `budget`;
/// a malformed file, or a clock before the file's, refuses (fail closed).
pub fn spend(file: &Path, rate: &Rate, now: u64) -> Res<()> {
    let (tokens, at) = match crate::state::fs::read_json(file)? {
        None => (rate.capacity, now),
        Some(v) => match (
            v.get("tokens").and_then(Value::as_u64),
            v.get("at").and_then(Value::as_u64),
        ) {
            (Some(t), Some(a)) if a <= now => (t.min(rate.capacity), a),
            _ => return Err(Refusal::internal("the team's spawn budget is malformed")),
        },
    };
    let earned = (now - at) / rate.refill;
    let tokens = (tokens + earned).min(rate.capacity);
    let at = if tokens == rate.capacity {
        now
    } else {
        at + earned * rate.refill
    };
    if tokens == 0 {
        let n = at + rate.refill - now;
        return refuse(
            2,
            "budget",
            format!("the team's spawn budget is spent; the next token arrives in {n}s"),
        );
    }
    let dir = file.parent().unwrap_or(Path::new(""));
    // The folders this call makes, nearest first; each is a new entry in its parent.
    let made = dir
        .ancestors()
        .take_while(|a| !a.as_os_str().is_empty() && !a.exists())
        .count();
    std::fs::create_dir_all(dir)?;
    crate::state::fs::write_json_durable(
        file,
        &json!({"schema": 1, "tokens": tokens - 1, "at": at}),
    )?;
    // write_json_durable flushed `dir`; `dir`'s own entry, and every folder made here, live in the parents (#103 D5:
    // the debit outlives a failed launch). A failed flush is an error, so nothing launches.
    for parent in dir
        .ancestors()
        .skip(1)
        .take(made.max(1))
        .filter(|p| !p.as_os_str().is_empty())
    {
        crate::state::fs::sync_dir(parent)?;
    }
    Ok(())
}
/// The clock in whole seconds since the epoch.
pub fn now_secs(ctx: &Ctx) -> u64 {
    ctx.clock
        .now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::fixture::World;
    use crate::testing::{Fakes, TempDir};
    #[test]
    fn the_21st_spend_waits_for_its_refill() {
        let d = TempDir::new();
        let file = d.path().join("teams").join("x.budget.json");
        for _ in 0..20 {
            spend(&file, &DEFAULT, 0).unwrap();
        }
        assert_eq!(
            spend(&file, &DEFAULT, 10),
            refuse(
                2,
                "budget",
                "the team's spawn budget is spent; the next token arrives in 170s"
            )
        );
        spend(&file, &DEFAULT, 180).unwrap();
        assert_eq!(
            spend(&file, &DEFAULT, 181).unwrap_err().message,
            "the team's spawn budget is spent; the next token arrives in 179s"
        );
        crate::state::fs::fail_durable_flush(Some(0));
        assert!(
            spend(&file, &DEFAULT, 10_000).is_err(),
            "a failed flush is an error, so nothing launches"
        );
        crate::state::fs::fail_durable_flush(None);
    }
    #[test]
    fn a_new_teams_folder_is_flushed_into_its_server_folder() {
        let d = TempDir::new();
        let server = d.path().join("server");
        std::fs::create_dir(&server).unwrap();
        let file = server.join("teams").join("x.budget.json");
        crate::state::fs::fail_sync_dir(Some(&server));
        let failed = spend(&file, &DEFAULT, 0);
        crate::state::fs::fail_sync_dir(None);
        assert!(
            failed.is_err(),
            "the new teams entry is not on disk, so nothing launches"
        );
        spend(&file, &DEFAULT, 0).unwrap();
        let v = crate::state::fs::read_json(&file).unwrap().unwrap();
        assert_eq!(
            (v["tokens"].as_u64(), v["at"].as_u64()),
            (Some(18), Some(0)),
            "a debit not proven durable still counts: fail closed"
        );
    }
    #[test]
    fn options_set_the_rate() {
        let w = World::human("");
        assert_eq!(rate(&w.ctx, &w.caller()), DEFAULT);
        w.fakes.mux_rule(
            "@mux-agents-spawn-budget",
            0,
            "2
",
            "",
        );
        w.fakes.mux_rule(
            "@mux-agents-spawn-refill",
            0,
            "60
",
            "",
        );
        let (r, file) = (rate(&w.ctx, &w.caller()), w.ctx.cwd.join("b.json"));
        assert_eq!(
            r,
            Rate {
                capacity: 2,
                refill: 60
            }
        );
        for _ in 0..2 {
            spend(&file, &r, 0).unwrap();
        }
        assert_eq!(
            spend(&file, &r, 10).unwrap_err().message,
            "the team's spawn budget is spent; the next token arrives in 50s",
            "the refill option sets the wait"
        );
        spend(&file, &r, 60).unwrap();
    }
    #[test]
    fn concurrent_spends_never_exceed_the_capacity() {
        let d = TempDir::new();
        let (file, locks) = (
            d.path().join("teams").join("x.budget.json"),
            d.path().join("locks"),
        );
        let wins: usize = std::thread::scope(|s| {
            let all: Vec<_> = (0..25)
                .map(|_| {
                    s.spawn(|| {
                        let f = Fakes::default();
                        let ctx = f.ctx();
                        f.real_locks(&locks);
                        usize::from(
                            crate::agents::lock(&ctx, "spawn|k|$3", || spend(&file, &DEFAULT, 0))
                                .is_ok(),
                        )
                    })
                })
                .collect();
            all.into_iter().map(|h| h.join().unwrap()).sum()
        });
        assert_eq!(wins, 20);
    }
}
