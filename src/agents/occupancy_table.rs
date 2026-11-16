//! occupy-3 H1, H4.3-H4.5, calls 10/20: the rules for one unmanaged row, client images, and /proc's view.
use super::*;

/// One unmanaged row: H4.3 argv, H4.4 a client of `kind`, H4.5 unbound unless its image names no client of `kind`.
pub fn holds(
    os_calls: &dyn crate::proc::Os,
    row: &Row,
    session: &str,
    kind: Client,
    os: OsKind,
) -> Option<Hold> {
    if row
        .argv
        .as_deref()
        .is_some_and(|argv| occupies(argv, session))
    {
        return Some(Hold::of(Rule::Argv, row));
    }
    // o3 call 34 N2: a client identity that cannot be resolved is unbound too, judged by its image.
    let known = os_calls.client_known(row);
    if known == Some(Some(kind)) {
        return Some(Hold::of(Rule::Unmanaged, row));
    }
    let unbound =
        known.is_none() || row.start.is_none() || row.path.is_none() || row.argv.is_none();
    let cleared = row
        .image
        .as_deref()
        .is_some_and(|image| !client_image(image, kind, os));
    (unbound && !cleared).then(|| Hold::of(Rule::Unbound, row))
}

/// Whether an image name may be a client of `kind` (ambiguity 3): Unix exact and case-sensitive (`claude`; `copilot`
/// or `node`); Windows by `parse::windows_client` (any case, `.exe.old*` kept), plus `node.exe` for Copilot.
pub fn client_image(image: &str, kind: Client, os: OsKind) -> bool {
    match os {
        OsKind::Unix => match kind {
            Client::Claude => image == "claude",
            Client::Copilot => image == "copilot" || image == "node",
        },
        OsKind::Windows => {
            crate::parse::windows_client(image) == Some(kind)
                || (kind == Client::Copilot && image.eq_ignore_ascii_case("node.exe"))
        }
    }
}

/// Calls 10, 20: what /proc's mounts let occupancy see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcView {
    Open,
    Hidden,
    Unreadable,
}

/// Calls 10, 20: fail closed on any doubt. Every mountinfo line whose mount point (field 5) is `/proc` counts: more
/// than one (stacked, in any order), one that is not `proc` (a tmpfs over it), one that cannot be split, or one with
/// `hidepid` other than `0`/`off` or `subset=pid` in its mount options (field 6) or its super options (after ` - `:
/// fstype, source, super options) is Hidden. No `/proc` line is Unreadable. One plain `proc` at /proc is Open.
pub fn proc_view(mountinfo: &str) -> ProcView {
    let mut at_proc = 0;
    let mut doubt = false;
    for line in mountinfo.lines() {
        let mut fields = line.split(' ');
        if fields.nth(4) != Some("/proc") {
            continue;
        }
        at_proc += 1;
        let mount_opts = fields.next().unwrap_or("");
        let Some((_, tail)) = line.split_once(" - ") else {
            doubt = true;
            continue;
        };
        let mut tail = tail.split(' ');
        let (fstype, super_opts) = (tail.next(), tail.nth(1).unwrap_or(""));
        let hides = mount_opts.split(',').chain(super_opts.split(',')).any(|o| {
            o == "subset=pid"
                || o.strip_prefix("hidepid=")
                    .is_some_and(|v| v != "0" && v != "off")
        });
        doubt |= fstype != Some("proc") || hides;
    }
    match at_proc {
        0 => ProcView::Unreadable,
        1 if !doubt => ProcView::Open,
        _ => ProcView::Hidden,
    }
}

/// Calls 10, 20, before any table read: a doubtful /proc holds as `Hidden`; mountinfo that cannot be read, or that
/// names no /proc, as `Table`. No seam (not Linux): nothing.
pub(super) fn proc_hold(ctx: &Ctx) -> Option<Hold> {
    match ctx.os.proc_mounts()? {
        Err(_) => Some(Hold::machine(Rule::Table)),
        Ok(text) => match proc_view(&text) {
            ProcView::Open => None,
            ProcView::Hidden => Some(Hold::machine(Rule::Hidden)),
            ProcView::Unreadable => Some(Hold::machine(Rule::Table)),
        },
    }
}

/// Whether a process with `argv` runs a client on `session`: the uuid follows `--session-id` (or
/// `--session-id=`), or `--resume`, `-r` (or `--resume=`) in a command line without `--fork-session`. A Claude fork
/// (`--resume <source> --fork-session --session-id <new>`) occupies only the new conversation; a Copilot fork resumes
/// its own copy under the new id, so its `--resume` already names only the new one. Words after `--` are no flags.
pub(crate) fn occupies(argv: &[String], session: &str) -> bool {
    let flags: Vec<&str> = argv
        .iter()
        .map(String::as_str)
        .take_while(|a| *a != "--")
        .collect();
    let fork = flags.contains(&"--fork-session");
    flags.iter().enumerate().any(|(i, a)| {
        let (name, inline) = match a.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (*a, None),
        };
        let resume = name == "--resume" || (name == "-r" && inline.is_none());
        if !(name == "--session-id" || (resume && !fork)) {
            return false;
        }
        inline.or_else(|| flags.get(i + 1).copied()) == Some(session)
    })
}
