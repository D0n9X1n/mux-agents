//! The live-session registry (#103 report-2b K2): `<state root>/live/<kind>-<pid>-<start>.json` per client process,
//! `{kind, pid, start, sessionId, seq, source, at}`, written under `live|<pid>-<start>`; the highest `seq` is the
//! newest report. An entry whose pid and start name no live process is ignored, and `sweep` removes it.
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::ctx::Ctx;
use crate::error::{Refusal, Res};
use crate::parse::Client;
use crate::state::evidence;

pub fn dir(ctx: &Ctx) -> PathBuf {
    ctx.state_root.join("live")
}

pub fn path(ctx: &Ctx, kind: Client, pid: u32, start: i64) -> PathBuf {
    dir(ctx).join(format!(
        "{}-{pid}-{start}.json",
        crate::agents::spawn::client_name(kind)
    ))
}

/// Ambiguity 7: per client process, with no server key; the registry belongs to the state root.
pub fn lock_key(pid: u32, start: i64) -> String {
    format!("live|{pid}-{start}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alive {
    Live,
    Dead,
    Unknown,
}

/// `<kind>-<pid>-<start>.json` exactly as `path` writes it (no sign, no leading zero), else None.
pub fn parse_name(name: &str) -> Option<(Client, u32, i64)> {
    use crate::agents::spawn::{client_name, client_named};
    let mut parts = name.strip_suffix(".json")?.splitn(3, '-');
    let kind = client_named(parts.next()?)?;
    let pid: u32 = parts.next()?.parse().ok()?;
    let start: i64 = parts.next()?.parse().ok().filter(|s| *s >= 0)?;
    (format!("{}-{pid}-{start}.json", client_name(kind)) == name).then_some((kind, pid, start))
}

/// K2: under `live|<pid>-<start>`, `seq` = the entry's `seq` + 1 (1 when absent), written durably. An unreadable or
/// wrong-type entry, a non-numeric `seq`, a `seq` at `u64::MAX` (PM call 12), or a `live/` that is not a real folder
/// refuses `evidence` and writes nothing.
pub fn report(
    ctx: &Ctx,
    kind: Client,
    pid: u32,
    start: i64,
    session: &str,
    source: &str,
) -> Res<u64> {
    if start < 0 {
        return Err(Refusal::internal(
            "a negative start names no registry entry",
        ));
    }
    // r2b call 29: where starts are boot-relative, an entry is written only with the boot it belongs to; an unreadable
    // boot refuses before anything is written, so an existing entry keeps its boot.
    let boot = match ctx.os.boot() {
        crate::proc::Boot::Unreadable => return Err(evidence::unreadable()),
        crate::proc::Boot::Id(id) => Some(id),
        crate::proc::Boot::Absolute => None,
    };
    let file = path(ctx, kind, pid, start);
    #[cfg(test)]
    hooks::fire_before_report_lock();
    crate::agents::lock(ctx, &lock_key(pid, start), || {
        live_dir(ctx)?;
        let seq = match evidence::read(&file).known()? {
            None => 1,
            Some(old) => old
                .get("seq")
                .and_then(Value::as_u64)
                .and_then(|s| s.checked_add(1))
                .ok_or_else(evidence::unreadable)?,
        };
        private_dir(ctx)?;
        let at = crate::util::utc_o(ctx.clock.now());
        let mut entry = json!({"kind": crate::agents::spawn::client_name(kind), "pid": pid, "start": start,
            "sessionId": session, "seq": seq, "source": source, "at": at});
        // r2b call 27: where starts are boot-relative, the boot they belong to.
        if let Some(boot) = &boot {
            entry["boot"] = boot.clone().into();
        }
        crate::state::fs::write_json_durable(&file, &entry)?;
        Ok(seq)
    })
}

/// R5: true for a real `live/` folder, false when it does not exist; a link (a junction too), a file or anything else
/// refuses `evidence`.
fn live_dir(ctx: &Ctx) -> Res<bool> {
    match std::fs::symlink_metadata(dir(ctx)) {
        Ok(m) if m.is_dir() => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        _ => Err(evidence::unreadable()),
    }
}

/// `live/` made private as `agents::server_dir` makes its folders.
fn private_dir(ctx: &Ctx) -> Res<()> {
    let live = dir(ctx);
    #[cfg(unix)]
    crate::state::private_unix::private_dirs(&ctx.state_root, &[&live])?;
    #[cfg(windows)]
    crate::state::private_windows::private_dirs(&[&live])?;
    Ok(())
}

/// PM calls 10 and 17, ambiguity 6: the process table alone decides. Absent: Dead (on Windows, a process that exited
/// may still open by handle for a moment, but it has left the Toolhelp table). Listed with another known start: Dead
/// (reused). Listed with no start (on Windows, a process this user cannot open), or no table at all: Unknown.
pub fn alive(ctx: &Ctx, pid: u32, start: i64) -> Alive {
    let Ok(rows) = ctx.os.table() else {
        return Alive::Unknown;
    };
    match rows.iter().find(|r| r.pid == pid).map(|r| r.start) {
        None => Alive::Dead,
        Some(None) => Alive::Unknown,
        Some(Some(s)) if ctx.os.same_start(start, s) => Alive::Live,
        Some(Some(_)) => Alive::Dead,
    }
}

/// The entry of a client process that may be live (Live or Unknown); a dead one is ignored and never read. Where
/// starts are boot-relative (r2b calls 27, 29): an entry recorded in a known other boot is dead too (`Ok(None)`); an
/// entry without `boot`, or a current boot that cannot be read, is uncertain and refuses `evidence` (`Err`), never a
/// usable entry.
pub fn entry(ctx: &Ctx, kind: Client, pid: u32, start: i64) -> Res<Option<Value>> {
    if !live_dir(ctx)? || alive(ctx, pid, start) == Alive::Dead {
        return Ok(None);
    }
    let Some(found) = evidence::read(&path(ctx, kind, pid, start)).known()? else {
        return Ok(None);
    };
    match boot_match(ctx, &found) {
        BootMatch::This => Ok(Some(found)),
        BootMatch::Another => Ok(None),
        BootMatch::Uncertain => Err(evidence::unreadable()),
    }
}

/// What an entry's recorded `boot` says against this boot (r2b calls 27, 29).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BootMatch {
    /// Recorded in this boot, or starts are absolute (macOS, Windows), where a (pid, start) never repeats.
    This,
    /// Recorded in a known other boot: dead.
    Another,
    /// The current boot cannot be read, or the entry names no boot: decides nothing.
    Uncertain,
}

fn boot_match(ctx: &Ctx, entry: &Value) -> BootMatch {
    match ctx.os.boot() {
        crate::proc::Boot::Absolute => BootMatch::This,
        crate::proc::Boot::Unreadable => BootMatch::Uncertain,
        crate::proc::Boot::Id(now) => match entry.get("boot").and_then(Value::as_str) {
            Some(b) if b == now => BootMatch::This,
            Some(_) => BootMatch::Another,
            None => BootMatch::Uncertain,
        },
    }
}

/// K2: removes each regular entry whose process is Dead, rechecked under that entry's own lock. Other names, folders,
/// links and FIFOs are left (their type is read with `symlink_metadata`, never opened).
pub fn sweep(ctx: &Ctx) -> Res<usize> {
    if !live_dir(ctx)? {
        return Ok(0);
    }
    let mut removed = 0;
    for item in std::fs::read_dir(dir(ctx)).map_err(|_| evidence::unreadable())? {
        let name = item.map_err(|_| evidence::unreadable())?.file_name();
        let Some((kind, pid, start)) = name.to_str().and_then(parse_name) else {
            continue;
        };
        let file = path(ctx, kind, pid, start);
        removed += crate::agents::lock(ctx, &lock_key(pid, start), || {
            let regular = std::fs::symlink_metadata(&file).is_ok_and(|m| m.is_file());
            if !regular {
                return Ok(0);
            }
            // Dead in the table, or recorded in a known other boot (r2b calls 27, 29); a torn entry, an entry without a
            // boot, or an unreadable current boot decides nothing (kept).
            let dead = alive(ctx, pid, start) == Alive::Dead
                || matches!(evidence::read(&file),
                    evidence::Evidence::Present(v) if boot_match(ctx, &v) == BootMatch::Another);
            if !dead {
                return Ok(0);
            }
            match std::fs::remove_file(&file) {
                Ok(()) => Ok(1),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
                Err(_) => Err(evidence::unreadable()),
            }
        })?;
    }
    Ok(removed)
}

/// (fix 2, re-audit #2) Test seam: runs inside `report`, right before it asks for the entry lock, so a race test can
/// mark the moment of the ask rather than the moment `report` was called.
#[cfg(test)]
pub(crate) mod hooks {
    use std::cell::RefCell;
    type Hook = Box<dyn Fn()>;
    thread_local! {
        static BEFORE: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }
    pub fn before_report_lock(hook: Option<Hook>) {
        BEFORE.with(|b| *b.borrow_mut() = hook);
    }
    pub(super) fn fire_before_report_lock() {
        BEFORE.with(|b| {
            if let Some(hook) = b.borrow().as_ref() {
                hook()
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{
        Fakes, TempDir, Wrong, plant, private_tree, row, unknown_start, untouched,
    };
    use std::cell::RefCell;
    use std::rc::Rc;

    const SID: &str = "0f11fc2f-2032-48ef-a920-4af2a8e5af50";

    fn world() -> (Fakes, Ctx, TempDir) {
        let f = Fakes::default();
        f.process(row(150, 100, 1500));
        let mut ctx = f.ctx();
        let d = TempDir::new();
        ctx.state_root = d.path().join("state");
        (f, ctx, d)
    }

    fn file(ctx: &Ctx) -> PathBuf {
        path(ctx, Client::Claude, 150, 1500)
    }

    /// (fix 1) The refusal's check, so a RED run or a mutation prints a value, never an unwrap panic.
    fn put(ctx: &Ctx, pid: u32, start: i64, source: &str) -> Result<u64, &'static str> {
        report(ctx, Client::Claude, pid, start, SID, source).map_err(|r| r.check)
    }

    fn checked<T>(r: Res<T>) -> Result<T, &'static str> {
        r.map_err(|r| r.check)
    }

    #[test]
    fn a_report_writes_its_entry_and_counts_seq() {
        let (f, ctx, _d) = world();
        let log = Rc::new(RefCell::new(Vec::new()));
        let l = log.clone();
        f.lock_probe(move |e| {
            l.borrow_mut().push(e.to_string());
            String::new()
        });
        assert_eq!(put(&ctx, 150, 1500, "startup"), Ok(1));
        let v = crate::state::fs::read_json(&file(&ctx)).unwrap().unwrap();
        assert_eq!(
            (
                &v["kind"],
                &v["pid"],
                &v["start"],
                &v["sessionId"],
                &v["seq"],
                &v["source"]
            ),
            (
                &json!("claude"),
                &json!(150),
                &json!(1500),
                &json!(SID),
                &json!(1),
                &json!("startup")
            )
        );
        assert!(v["at"].as_str().and_then(crate::util::parse_utc).is_some());
        assert_eq!(put(&ctx, 150, 1500, "resume"), Ok(2));
        let v = crate::state::fs::read_json(&file(&ctx)).unwrap().unwrap();
        assert_eq!((&v["seq"], &v["source"]), (&json!(2), &json!("resume")));
        assert_eq!(
            *log.borrow(),
            [
                "+live|150-1500",
                "-live|150-1500",
                "+live|150-1500",
                "-live|150-1500"
            ]
        );
        assert_eq!(file(&ctx).file_name().unwrap(), "claude-150-1500.json");
        assert_eq!(
            parse_name("claude-150-1500.json"),
            Some((Client::Claude, 150, 1500))
        );
        let bad = [
            "claude-150-01500.json",
            "claude-150.json",
            "codex-1-2.json",
            "claude-1--2.json",
            "claude-1-2.txt",
        ];
        for name in bad {
            assert_eq!(parse_name(name), None, "{name}");
        }
    }

    // R2 and PM call 12: a torn entry, a non-numeric seq or a seq at u64::MAX refuses and is left as it is.
    #[test]
    fn an_unreadable_entry_or_a_full_seq_is_never_overwritten() {
        let (_f, ctx, _d) = world();
        assert_eq!(put(&ctx, 150, 1500, "startup"), Ok(1), "seed");
        let max = format!(r#"{{"seq":{}}}"#, u64::MAX);
        for bytes in [b"{".to_vec(), br#"{"seq":"x"}"#.to_vec(), max.into_bytes()] {
            std::fs::write(file(&ctx), &bytes).unwrap();
            let shown = String::from_utf8_lossy(&bytes).into_owned();
            assert_eq!(put(&ctx, 150, 1500, "resume"), Err("evidence"), "{shown}");
            assert_eq!(
                std::fs::read(file(&ctx)).unwrap(),
                bytes,
                "{shown}: never overwritten"
            );
        }
        // o3 call 6: `entry` of a live process whose file is torn is `Err`, never "no entry".
        std::fs::write(file(&ctx), b"{").unwrap();
        let torn = checked(entry(&ctx, Client::Claude, 150, 1500));
        assert_eq!(torn, Err("evidence"), "entry: a torn entry");
        assert_eq!(
            put(&ctx, 160, -1, "startup"),
            Err("internal"),
            "a negative start names no file"
        );
        assert!(!dir(&ctx).join("claude-160--1.json").exists());
    }

    // PM call 13: a wrong file type at the entry or at `live/` refuses `evidence`, is never written through, and
    // sweep leaves it.
    #[test]
    fn a_wrong_type_entry_or_live_folder_is_refused_and_left() {
        for kind in Wrong::all() {
            let (_f, ctx, _d) = world();
            assert_eq!(put(&ctx, 150, 1500, "startup"), Ok(1), "{kind:?}: seed");
            std::fs::remove_file(file(&ctx)).unwrap();
            plant(kind, &file(&ctx));
            assert_eq!(
                put(&ctx, 150, 1500, "resume"),
                Err("evidence"),
                "{kind:?}: report"
            );
            assert_eq!(
                checked(entry(&ctx, Client::Claude, 150, 1500)),
                Err("evidence"),
                "{kind:?}: entry"
            );
            assert!(untouched(&file(&ctx)), "{kind:?}: nothing written through");
            let dead = path(&ctx, Client::Claude, 700, 7000); // pid 700 is not listed: Dead
            plant(kind, &dead);
            assert_eq!(
                checked(sweep(&ctx)),
                Ok(0),
                "{kind:?}: sweep removes no wrong type"
            );
            assert!(std::fs::symlink_metadata(&dead).is_ok(), "{kind:?}: left");
        }
        for kind in Wrong::all().into_iter().filter(|k| *k != Wrong::Folder) {
            let (_f, ctx, _d) = world();
            private_tree(&ctx.state_root, &ctx.state_root);
            plant(kind, &dir(&ctx));
            assert_eq!(
                put(&ctx, 150, 1500, "startup"),
                Err("evidence"),
                "{kind:?} at live/: report"
            );
            assert_eq!(
                checked(sweep(&ctx)),
                Err("evidence"),
                "{kind:?} at live/: sweep"
            );
            assert_eq!(
                checked(entry(&ctx, Client::Claude, 150, 1500)),
                Err("evidence"),
                "{kind:?} at live/"
            );
            assert!(
                untouched(&dir(&ctx)),
                "{kind:?} at live/: nothing written through"
            );
        }
        let (_f, ctx, _d) = world();
        private_tree(&ctx.state_root, &ctx.state_root);
        std::fs::write(dir(&ctx), b"x").unwrap();
        assert_eq!(
            put(&ctx, 150, 1500, "startup"),
            Err("evidence"),
            "a file at live/"
        );
        assert_eq!(
            checked(sweep(&ctx)),
            Err("evidence"),
            "a file at live/: sweep"
        );
    }

    // r2b call 5 closed by occupy-3: a Unix pid kept with no start (`seam::listed`) reads Unknown, never Dead, and
    // sweep keeps its entry.
    #[test]
    fn a_kept_unix_row_without_a_start_is_unknown() {
        let (f, ctx, _d) = world();
        assert_eq!(put(&ctx, 150, 1500, "startup"), Ok(1), "seed");
        f.forget(150);
        f.process(crate::proc::Row {
            ppid: 0,
            start: None,
            path: None,
            argv: None,
            ..row(150, 100, 0)
        });
        assert_eq!(alive(&ctx, 150, 1500), Alive::Unknown);
        assert_eq!(checked(sweep(&ctx)), Ok(0));
        assert!(file(&ctx).is_file());
    }

    // PM call 10: Dead only when the pid is absent from the process table, or listed with another known start.
    // Cannot open or query (Windows' denied OpenProcess, `Fakes::deny`) is Unknown, and the entry is kept.
    #[test]
    fn dead_only_when_absent_or_listed_with_another_start() {
        let (f, ctx, _d) = world();
        assert_eq!(put(&ctx, 150, 1500, "startup"), Ok(1), "seed");
        assert_eq!(alive(&ctx, 150, 1500), Alive::Live);
        assert!(
            checked(entry(&ctx, Client::Claude, 150, 1500))
                .unwrap()
                .is_some()
        );
        f.deny(150);
        assert_eq!(
            alive(&ctx, 150, 1500),
            Alive::Unknown,
            "listed, cannot be opened"
        );
        assert!(
            checked(entry(&ctx, Client::Claude, 150, 1500))
                .unwrap()
                .is_some(),
            "kept for occupy-3"
        );
        assert_eq!(checked(sweep(&ctx)), Ok(0), "denied: kept");
        assert!(file(&ctx).exists());
        f.table_fails();
        assert_eq!(
            alive(&ctx, 150, 1500),
            Alive::Unknown,
            "no table: nothing decided"
        );
        let (f, ctx, _d) = world();
        assert_eq!(put(&ctx, 150, 1500, "startup"), Ok(1), "seed");
        f.forget(150);
        f.process(row(150, 100, 1600));
        assert_eq!(alive(&ctx, 150, 1500), Alive::Dead, "a reused pid");
        assert_eq!(checked(entry(&ctx, Client::Claude, 150, 1500)), Ok(None));
        f.forget(150);
        assert_eq!(alive(&ctx, 150, 1500), Alive::Dead, "gone");
        f.process(unknown_start(150, 100));
        assert_eq!(
            alive(&ctx, 150, 1500),
            Alive::Unknown,
            "listed start unreadable"
        );
        assert!(file(&ctx).exists(), "a reader never deletes");
    }

    #[test]
    fn sweep_removes_only_dead_entries_and_leaves_other_names() {
        let (f, ctx, _d) = world();
        assert_eq!(
            checked(sweep(&ctx)),
            Ok(0),
            "no live/ folder: nothing to sweep"
        );
        f.process(row(800, 1, 9000));
        f.process(unknown_start(900, 1));
        f.process(row(950, 1, 9500));
        for (pid, start) in [
            (150, 1500),
            (700, 7000),
            (800, 8000),
            (900, 9000),
            (950, 9500),
        ] {
            assert_eq!(put(&ctx, pid, start, "startup"), Ok(1), "seed {pid}");
        }
        f.deny(950);
        std::fs::write(dir(&ctx).join("notes.txt"), b"x").unwrap();
        std::fs::create_dir(dir(&ctx).join("copilot-1-2.json")).unwrap();
        assert_eq!(
            checked(sweep(&ctx)),
            Ok(2),
            "700 gone, 800 reused; 900 unknown and 950 denied kept"
        );
        let mut left: Vec<String> = std::fs::read_dir(dir(&ctx))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        let want = [
            "claude-150-1500.json",
            "claude-900-9000.json",
            "claude-950-9500.json",
            "copilot-1-2.json",
            "notes.txt",
        ];
        assert_eq!(left, want);
    }

    // 700 comes back to life the moment its entry lock is taken: only a check made under the lock sees it.
    #[test]
    fn sweep_rechecks_liveness_under_each_entry_lock() {
        let (f, ctx, _d) = world();
        assert_eq!(put(&ctx, 700, 7000, "startup"), Ok(1), "seed");
        let (back, log) = (f.clone(), Rc::new(RefCell::new(Vec::new())));
        let l = log.clone();
        f.lock_probe(move |e| {
            l.borrow_mut().push(e.to_string());
            if e == "+live|700-7000" {
                back.process(row(700, 1, 7000));
            }
            String::new()
        });
        assert_eq!(
            checked(sweep(&ctx)),
            Ok(0),
            "alive again when the lock was taken"
        );
        assert!(path(&ctx, Client::Claude, 700, 7000).exists());
        assert_eq!(
            *log.borrow(),
            ["+live|700-7000", "-live|700-7000"],
            "the recheck is under the entry lock"
        );
    }

    // (fix 2) PM call 17: the table alone decides. A pid the table no longer lists is Dead, even while its row
    // still opens with the recorded start; an `alive` that asks the row or the handle first keeps it.
    #[test]
    fn a_pid_absent_from_the_table_is_dead_even_while_its_handle_opens() {
        let (f, ctx, _d) = world();
        assert_eq!(put(&ctx, 150, 1500, "startup"), Ok(1), "seed");
        f.unlist(150);
        assert!(
            ctx.os.row(150).is_some() && ctx.os.open(150).is_some(),
            "precondition: the handle still answers"
        );
        assert_eq!(alive(&ctx, 150, 1500), Alive::Dead);
        assert_eq!(checked(sweep(&ctx)), Ok(1), "swept");
        assert!(!file(&ctx).exists());
    }

    // r2b call 27 (QA #143): where starts are boot-relative (Linux), an entry records its boot, and an entry from
    // another boot is dead to `entry` and `sweep` even when the table lists the same pid with the same start. An
    // unreadable boot decides nothing (Unknown, kept). Absolute starts (macOS, Windows) record no boot.
    #[test]
    fn an_entry_from_another_boot_is_dead_and_an_unreadable_boot_holds() {
        use crate::proc::Boot;
        let (f, ctx, _d) = world();
        f.set_boot(Boot::Id("boot-a".into()));
        assert_eq!(put(&ctx, 150, 1500, "startup"), Ok(1), "seed");
        let v = crate::state::fs::read_json(&file(&ctx)).unwrap().unwrap();
        assert_eq!(
            v["boot"],
            json!("boot-a"),
            "the boot is recorded where starts are boot-relative"
        );
        assert!(
            checked(entry(&ctx, Client::Claude, 150, 1500))
                .unwrap()
                .is_some(),
            "same boot"
        );
        assert_eq!(checked(sweep(&ctx)), Ok(0), "same boot: kept");
        f.set_boot(Boot::Unreadable);
        // r2b call 29: an unreadable current boot is uncertain, never usable evidence.
        assert_eq!(
            checked(entry(&ctx, Client::Claude, 150, 1500)),
            Err("evidence"),
            "unreadable boot: uncertain"
        );
        assert_eq!(
            checked(sweep(&ctx)),
            Ok(0),
            "unreadable boot: kept by sweep"
        );
        f.set_boot(Boot::Id("boot-b".into()));
        assert_eq!(
            alive(&ctx, 150, 1500),
            Alive::Live,
            "the table alone cannot tell the boots apart"
        );
        assert_eq!(
            checked(entry(&ctx, Client::Claude, 150, 1500)),
            Ok(None),
            "another boot: dead to entry"
        );
        assert_eq!(checked(sweep(&ctx)), Ok(1), "another boot: swept");
        assert!(!file(&ctx).exists());
        let (f, ctx, _d) = world();
        f.set_boot(Boot::Absolute);
        assert_eq!(put(&ctx, 150, 1500, "startup"), Ok(1), "seed (absolute)");
        let v = crate::state::fs::read_json(&file(&ctx)).unwrap().unwrap();
        assert!(
            v.get("boot").is_none(),
            "absolute starts record no boot: {v}"
        );
    }

    // r2b call 29 (QA #143 re-review): where starts are boot-relative, a report under an unreadable boot refuses and
    // writes nothing, so the entry keeps its boot; an entry is usable only when its boot is known to be this boot.
    #[test]
    fn an_unreadable_boot_or_an_entry_without_one_is_never_usable() {
        use crate::proc::Boot;
        // QA repro: boot-a, then a report under an unreadable boot, then boot-b with the same pid and start.
        let (f, ctx, _d) = world();
        f.set_boot(Boot::Id("boot-a".into()));
        assert_eq!(put(&ctx, 150, 1500, "startup"), Ok(1), "seed");
        let before = std::fs::read(file(&ctx)).unwrap();
        f.set_boot(Boot::Unreadable);
        assert_eq!(
            put(&ctx, 150, 1500, "resume"),
            Err("evidence"),
            "unreadable boot: report refuses"
        );
        assert_eq!(
            std::fs::read(file(&ctx)).unwrap(),
            before,
            "the entry keeps its boot"
        );
        assert_eq!(
            checked(sweep(&ctx)),
            Ok(0),
            "unreadable boot: kept by sweep"
        );
        f.set_boot(Boot::Id("boot-b".into()));
        assert_eq!(
            checked(entry(&ctx, Client::Claude, 150, 1500)),
            Ok(None),
            "boot-a read in boot-b"
        );
        assert_eq!(checked(sweep(&ctx)), Ok(1), "boot-a swept in boot-b");
        // An entry that names no boot, read where starts are boot-relative: uncertain, kept.
        let (f, ctx, _d) = world();
        f.set_boot(Boot::Absolute);
        assert_eq!(
            put(&ctx, 150, 1500, "startup"),
            Ok(1),
            "seed without a boot"
        );
        f.set_boot(Boot::Id("boot-a".into()));
        assert_eq!(
            checked(entry(&ctx, Client::Claude, 150, 1500)),
            Err("evidence"),
            "an entry without a boot: uncertain"
        );
        assert_eq!(checked(sweep(&ctx)), Ok(0), "an entry without a boot: kept");
        assert!(file(&ctx).exists());
    }

    // (fix 2, re-audit #2) Contention proved from inside `report`: `hooks::before_report_lock` (`-8.md` Step 3) marks
    // the moment the writer asks for 150's lock. The sweeper holds that lock until the ask and 400 ms more. The writer
    // must ask while it is held and take it after the release and within 250 ms of it: a writer that reached the lock
    // call only after the release cannot.
    #[test]
    fn sweep_racing_a_live_report_on_real_locks_removes_only_dead_entries() {
        use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};
        type Marks = Arc<Mutex<Vec<(&'static str, Instant)>>>;
        fn mark(m: &Marks, what: &'static str) {
            m.lock().unwrap().push((what, Instant::now()));
        }
        let d = TempDir::new();
        let (state, locks) = (d.path().join("state"), d.path().join("locks"));
        let make = |f: &Fakes, state: &PathBuf, locks: &PathBuf| {
            f.process(row(150, 100, 1500));
            f.real_locks(locks);
            let mut ctx = f.ctx();
            ctx.state_root = state.clone();
            ctx
        };
        {
            let f = Fakes::default();
            let ctx = make(&f, &state, &locks);
            assert_eq!(put(&ctx, 150, 1500, "startup"), Ok(1), "seed 150");
            assert_eq!(put(&ctx, 700, 7000, "startup"), Ok(1), "seed 700");
        }
        let (held, asked) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let marks: Marks = Arc::new(Mutex::new(Vec::new()));
        let sweeper = {
            let (s, l, h, a, m) = (
                state.clone(),
                locks.clone(),
                held.clone(),
                asked.clone(),
                marks.clone(),
            );
            std::thread::spawn(move || {
                let f = Fakes::default();
                let ctx = make(&f, &s, &l);
                f.lock_probe(move |e| {
                    if e == "+live|150-1500" {
                        h.store(true, SeqCst);
                        let end = Instant::now() + Duration::from_secs(10);
                        while !a.load(SeqCst) && Instant::now() < end {
                            std::thread::yield_now();
                        }
                        std::thread::sleep(Duration::from_millis(400));
                    } else if e == "-live|150-1500" {
                        mark(&m, "sweep releasing");
                    }
                    String::new()
                });
                checked(sweep(&ctx))
            })
        };
        let writer = {
            let (s, l, h, a, m) = (
                state.clone(),
                locks.clone(),
                held.clone(),
                asked.clone(),
                marks.clone(),
            );
            std::thread::spawn(move || {
                let end = Instant::now() + Duration::from_secs(10);
                while !h.load(SeqCst) && Instant::now() < end {
                    std::thread::yield_now();
                }
                let f = Fakes::default();
                let ctx = make(&f, &s, &l);
                let m2 = m.clone();
                hooks::before_report_lock(Some(Box::new(move || {
                    mark(&m2, "report asking");
                    a.store(true, SeqCst);
                })));
                // Build checklist item 7: contention proved from inside the lock's wait, not by timing.
                let (m3, once) = (m.clone(), std::cell::Cell::new(false));
                crate::state::lock::hooks::when_held(Some(Box::new(move || {
                    if !once.replace(true) {
                        mark(&m3, "report waited on a held lock");
                    }
                })));
                f.lock_probe(move |e| {
                    if e == "+live|150-1500" {
                        mark(&m, "report took");
                    }
                    String::new()
                });
                let got = put(&ctx, 150, 1500, "resume");
                hooks::before_report_lock(None);
                crate::state::lock::hooks::when_held(None);
                got
            })
        };
        assert_eq!(sweeper.join().unwrap(), Ok(1), "only dead 700");
        assert_eq!(
            writer.join().unwrap(),
            Ok(2),
            "the live entry was kept and counted on"
        );
        let marks = marks.lock().unwrap().clone();
        let names: Vec<&str> = marks.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names,
            [
                "report asking",
                "report waited on a held lock",
                "sweep releasing",
                "report took"
            ],
            "asked while held, waited on it, took after release"
        );
        let gap = marks[3].1.duration_since(marks[2].1);
        assert!(
            gap < Duration::from_millis(250),
            "the report was waiting at the lock when it was released: {gap:?}"
        );
        let f = Fakes::default();
        let ctx = make(&f, &state, &locks);
        assert!(file(&ctx).exists());
        assert!(!path(&ctx, Client::Claude, 700, 7000).exists());
    }
}
