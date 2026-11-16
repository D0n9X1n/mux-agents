//! The one walk from a process to the Claude or Copilot client above it (#103 report-2b K3; D1's caller walk).
use crate::ctx::Ctx;
use crate::parse::Client;
use crate::proc::Row;
use crate::state::root::OsKind;

/// What sits above a process: its first client, no client, or a walk that cannot be trusted (R2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Above {
    Client(Row, Client),
    None,
    Unreadable,
}

/// Parents read before the walk gives up (the caller walk's cap, `caller::MAX_PARENTS`).
const MAX_PARENTS: usize = 64;

/// The loop `prove_member` had (`tree.rs:170-183`), with the same OS calls in the same order: this process's own row,
/// then each parent once per step. A parent must be listed (pid 0 ends the walk with no client), with a start no newer
/// than its child's (else a reused pid). The first client ends the walk; a readable non-shell is `None`.
pub fn first_client(ctx: &Ctx) -> Above {
    let Some(mut row) = ctx.os.row(ctx.pid).filter(|r| r.start.is_some()) else {
        return Above::Unreadable;
    };
    for _ in 0..MAX_PARENTS {
        let Some(parent) = ctx.os.row(row.ppid) else {
            return if row.ppid == 0 {
                Above::None
            } else {
                Above::Unreadable
            };
        };
        if parent.start.is_none() || parent.start > row.start {
            return Above::Unreadable;
        }
        if let Some(client) = ctx.os.client_of(&parent) {
            return Above::Client(parent, client);
        }
        if !ctx.os.is_shell(&parent) {
            return Above::None;
        }
        row = parent;
    }
    Above::Unreadable
}

pub fn identity(ctx: &Ctx, found: &Row, client: Client) -> Option<(u32, i64)> {
    identity_on(ctx, found, client, crate::state::root::HOST)
}

fn file(row: &Row) -> Option<&str> {
    row.path
        .as_deref()
        .map(|p| p.rsplit('/').next().unwrap_or(p))
}

/// The (pid, start) a launcher records for `found`: on Unix, a native `copilot`'s npm loader (`loader_of`); else
/// `found`. The start is read through an open handle and must equal the listed one. None: unresolved, write nothing.
pub(crate) fn identity_on(
    ctx: &Ctx,
    found: &Row,
    client: Client,
    os: OsKind,
) -> Option<(u32, i64)> {
    let chosen =
        if client == Client::Copilot && os == OsKind::Unix && file(found) == Some("copilot") {
            loader_of(ctx, found, os)?
        } else {
            found.clone()
        };
    let listed = chosen.start?;
    let opened = ctx.os.open(chosen.pid)?.start;
    ctx.os
        .same_start(opened, listed)
        .then_some((chosen.pid, opened))
}

/// PM call 9: the parent of a native Unix `copilot` must be read whole (row, a start no newer than the native's,
/// path). A `node` is the loader only when `tree::copilot_pair` recognizes the pair; a `node` it does not recognize is
/// unresolved (None). Only a parent positively named something else leaves the native process.
fn loader_of(ctx: &Ctx, native: &Row, os: OsKind) -> Option<Row> {
    pair_parent(ctx, &ctx.os.row(native.ppid)?, native, os)
}

/// `loader_of`'s rule on a parent row the caller already holds (occupy-3 calls 8, 17, 18: occupancy passes the row
/// from its one table read): a start no newer than the native's; a `node` is the loader only when `copilot_pair`
/// recognizes the pair (else unresolved, None); a parent named otherwise leaves the native process.
pub(crate) fn pair_parent(ctx: &Ctx, parent: &Row, native: &Row, os: OsKind) -> Option<Row> {
    if parent.start.is_none() || parent.start > native.start {
        return None;
    }
    if file(parent)? != "node" {
        return Some(native.clone());
    }
    let pair = super::tree::copilot_pair(parent, native, os, |r| {
        ctx.os.client_of(r) == Some(Client::Copilot)
    });
    pair.then(|| parent.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Fakes, row, unknown_start};

    // own 4242 > 170 (shell) > 160 (shell) > 150 (claude) > 100 (pane) > 1.
    fn chain() -> Fakes {
        let f = Fakes::default();
        f.process(row(100, 1, 1000));
        f.process(row(150, 100, 1500));
        f.process(row(160, 150, 1600));
        f.process(row(170, 160, 1700));
        f.process(row(4242, 170, 2000));
        f.client(150, Client::Claude);
        f.shell(160);
        f.shell(170);
        f
    }

    fn pid(above: Above) -> Option<u32> {
        match above {
            Above::Client(r, Client::Claude) => Some(r.pid),
            _ => None,
        }
    }

    #[test]
    fn the_walk_passes_shells_to_the_first_client() {
        let f = chain();
        assert_eq!(pid(first_client(&f.ctx())), Some(150));
        let f = Fakes::default();
        f.process(row(150, 100, 1500));
        f.process(row(4242, 150, 2000));
        f.client(150, Client::Claude);
        assert_eq!(
            pid(first_client(&f.ctx())),
            Some(150),
            "a client right above"
        );
    }

    #[test]
    fn a_readable_non_shell_non_client_or_the_root_is_no_client() {
        assert_eq!(pid(first_client(&chain().ctx())), Some(150), "control");
        // 160 is listed and readable but neither a shell nor a client.
        let f = Fakes::default();
        f.process(row(150, 100, 1500));
        f.process(row(160, 150, 1600));
        f.process(row(170, 160, 1700));
        f.process(row(4242, 170, 2000));
        f.client(150, Client::Claude);
        f.shell(170);
        assert_eq!(first_client(&f.ctx()), Above::None, "non-shell");
        let f = Fakes::default();
        f.process(row(170, 0, 1700));
        f.process(row(4242, 170, 2000));
        f.shell(170);
        assert_eq!(first_client(&f.ctx()), Above::None, "root: pid 0");
    }

    #[test]
    fn a_missing_unknown_or_newer_parent_is_unreadable() {
        type Break = fn(&Fakes);
        let cases: [(&str, Break); 6] = [
            ("parent missing", |f| f.forget(160)),
            // (fix 1) The client itself has no start: `None > Some(1600)` is false, so only the is_none guard holds.
            ("client start unknown", |f| {
                f.forget(150);
                f.process(unknown_start(150, 100));
            }),
            ("parent newer than its child", |f| {
                f.forget(160);
                f.process(row(160, 150, 2500));
            }),
            ("own row missing", |f| f.forget(4242)),
            ("own start unknown", |f| {
                f.forget(4242);
                f.process(unknown_start(4242, 170));
            }),
            ("over 64 shells", |f| {
                f.forget(4242);
                f.process(row(4242, 300, 2000));
                for p in 300..371 {
                    f.process(row(p, p + 1, 1000));
                    f.shell(p);
                }
            }),
        ];
        for (case, break_one) in cases {
            let f = chain();
            assert_eq!(pid(first_client(&f.ctx())), Some(150), "{case}: before");
            break_one(&f);
            assert_eq!(first_client(&f.ctx()), Above::Unreadable, "{case}");
        }
    }

    // (fix 1) The exact OS calls of the extracted walk, pinned against the loop it replaces in `prove_member`
    // (`tree.rs:170-183`: `row(ctx.pid)`, then per parent `row(ppid)`, `client_of`, `is_shell`, at most 64 times).
    // A readable shell naming itself: the old loop read it again until the cap and found no client; so does this.
    // It is also the cap's test: 64 parents, never 65.
    #[test]
    fn a_self_parenting_shell_keeps_the_old_loops_calls_and_hits_the_cap() {
        let f = Fakes::default();
        f.process(row(170, 170, 1700));
        f.process(row(4242, 170, 2000));
        f.shell(170);
        assert_eq!(first_client(&f.ctx()), Above::Unreadable);
        let mut want = vec!["row 4242".to_string()];
        for _ in 0..64 {
            want.extend(["row 170", "client_of 170", "is_shell 170"].map(String::from));
        }
        assert_eq!(f.os_calls(), want);
    }

    /// (fix 2, re-audit #1) The loader at 140: (start, path, recognized by `client_of`), or no row.
    type Loader = Option<(Option<i64>, Option<&'static str>, bool)>;
    /// One identity case: its name, the loader, the (pid, start) recorded (build checklist items 1 and 6).
    type Case = (&'static str, Loader, Option<(u32, i64)>);

    /// own 4242 > native `copilot` 150 > `loader` at 140.
    fn copilot(loader: Loader) -> (Fakes, Row) {
        let at = |pid, ppid, start, path: Option<&str>| Row {
            pid,
            ppid,
            start,
            path: path.map(String::from),
            argv: None,
            image: None,
        };
        let f = Fakes::default();
        if let Some((start, path, recognized)) = loader {
            f.process(at(140, 100, start, path));
            if recognized {
                f.client(140, Client::Copilot);
            }
        }
        let native = at(150, 140, Some(1500), Some("/opt/bin/copilot"));
        f.process(native.clone());
        f.process(row(4242, 150, 2000));
        f.client(150, Client::Copilot);
        (f, native)
    }

    // Ambiguity 2 and PM call 9: on Unix the launcher records the npm loader; a loader that cannot be read or
    // recognized makes the identity unresolved (None: nothing is written); only a parent positively known not to be
    // a loader leaves the native process. Windows has no pair.
    #[test]
    fn a_native_copilot_is_its_loader_on_unix_and_an_unresolved_loader_is_unreadable() {
        const NODE: Option<&str> = Some("/usr/bin/node");
        let cases: [Case; 7] = [
            ("pair", Some((Some(1400), NODE, true)), Some((140, 1400))),
            ("loader row missing", None, None),
            ("loader start unknown", Some((None, NODE, true)), None),
            (
                "loader newer than the native",
                Some((Some(1600), NODE, true)),
                None,
            ),
            (
                "loader path unreadable",
                Some((Some(1400), None, true)),
                None,
            ),
            (
                "node not recognized as copilot",
                Some((Some(1400), NODE, false)),
                None,
            ),
            (
                "parent positively not a loader",
                Some((Some(1400), Some("/usr/bin/bash"), false)),
                Some((150, 1500)),
            ),
        ];
        for (case, loader, want) in cases {
            let (f, native) = copilot(loader);
            assert_eq!(
                identity_on(&f.ctx(), &native, Client::Copilot, OsKind::Unix),
                want,
                "{case}"
            );
        }
        let (f, native) = copilot(Some((Some(1400), NODE, true)));
        f.opens(140, &[Some(1399)]);
        assert_eq!(
            identity_on(&f.ctx(), &native, Client::Copilot, OsKind::Unix),
            None,
            "reused loader at open"
        );
        for loader in [Some((Some(1400), NODE, true)), None] {
            let (f, native) = copilot(loader);
            let got = identity_on(&f.ctx(), &native, Client::Copilot, OsKind::Windows);
            assert_eq!(got, Some((150, 1500)), "Windows: copilot.exe itself");
        }
        let (f, native) = copilot(None);
        f.opens(150, &[Some(1499)]);
        let got = identity_on(&f.ctx(), &native, Client::Copilot, OsKind::Windows);
        assert_eq!(got, None, "reused native at open");
    }
}
