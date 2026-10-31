//! The caller: its pane, its server's identity and its nearest Claude or Copilot client (MuxAgents.ps1:357-421).

use crate::ctx::Ctx;
use crate::error::{Refusal, Res};
use crate::parse::Client;
use crate::proc::Row;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Agent,
    Human,
}

/// The caller's client process: its start read through an open handle, after the walk listed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub pid: u32,
    pub start: i64,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub kind: Kind,
    pub client: Option<Client>,
    pub agent: Option<Agent>,
    /// The walked rows, the caller's own first and the pane's last; argv included, so it never leaves memory.
    pub chain: Vec<Row>,
    pub pane: String,
    pub window: String,
    pub session: String,
    pub server_pid: u32,
    pub server_start: i64,
    pub socket: String,
    pub server_key: String,
    pub agent_tag: String,
}

const PROBE: &str = "#{pid}|#{pane_pid}|#{session_id}|#{window_id}|#{@agent-id}";
// Rows above the caller the walk may read before it gives up on reaching the pane.
const MAX_PARENTS: usize = 64;

fn digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// `^%\d+$`; `$` also allowed one final LF in .NET, but a pane id never ends in one, so it is refused.
fn pane_id(pane: &str) -> bool {
    pane.strip_prefix('%').is_some_and(digits)
}

struct Probe<'a> {
    server_pid: &'a str,
    pane_pid: u32,
    session: &'a str,
    window: &'a str,
    tag: &'a str,
}

/// `^(\d+)\|(\d+)\|(\$\d+)\|(@\d+)\|(.*)$` after trailing CR and LF are cut: the tag runs to the end and may
/// hold bars but no LF.
fn probe_row(stdout: &str) -> Option<Probe<'_>> {
    let line = stdout.trim_end_matches(['\r', '\n']);
    let mut parts = line.splitn(5, '|');
    let (server_pid, pane_pid, session, window, tag) = (
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
    );
    let marked = |text: &str, mark: char| text.strip_prefix(mark).is_some_and(digits);
    if !digits(server_pid)
        || !digits(pane_pid)
        || !marked(session, '$')
        || !marked(window, '@')
        || tag.contains('\n')
    {
        return None;
    }
    Some(Probe {
        server_pid,
        pane_pid: pane_pid.parse().ok()?,
        session,
        window,
        tag,
    })
}

fn endpoint_refusal(message: &str) -> Refusal {
    Refusal {
        code: 1,
        check: "endpoint",
        message: message.into(),
    }
}

fn caller_refusal(code: u8, message: &str) -> Refusal {
    Refusal {
        code,
        check: "caller",
        message: message.into(),
    }
}

/// Refuses 1 `endpoint` for an unusable endpoint, pane or probe, 1 `caller` when the caller is not below its pane
/// or its agent's start is unreadable or changed, and 2 `caller` for a non-shell in between.
pub fn resolve(ctx: &Ctx) -> Res<Caller> {
    let endpoint = crate::mux::endpoint(&ctx.env, crate::state::root::HOST)
        .ok_or_else(|| endpoint_refusal("not inside an rmux pane"))?;
    let pane = ctx.env.get("TMUX_PANE").unwrap_or("");
    if !pane_id(pane) {
        return Err(endpoint_refusal("not inside an rmux pane"));
    }
    // rmux 0.10.0 answers a probe of a gone pane with exit 0 and empty fields, so the row itself is validated.
    // A typed refusal from connecting or a timeout keeps its own message.
    let out = crate::mux::native(ctx, &["display-message", "-p", "-t", pane, PROBE], true)?;
    let probe = (out.code == 0)
        .then(|| probe_row(&out.stdout))
        .flatten()
        .ok_or_else(|| endpoint_refusal("rmux endpoint did not answer"))?;
    // Compared as numbers: the endpoint's pid is already a number, and leading zeros name the same process.
    if probe.server_pid.parse::<u32>().ok() != Some(endpoint.server_pid) {
        return Err(endpoint_refusal("rmux server changed"));
    }
    // A restarted server can reuse the socket and pid, so the key includes its exact start.
    let server_start = ctx
        .os
        .open(endpoint.server_pid)
        .map(|opened| opened.start)
        .ok_or_else(|| endpoint_refusal("rmux server start is unreadable"))?;
    let server_key = crate::util::sha256_hex(format!(
        "{}|{}|{server_start}",
        endpoint.socket, endpoint.server_pid
    ))[..16]
        .to_string();

    let chain = walk(ctx, probe.pane_pid)?;
    // The nearest row above the caller that is a client is the agent; only shells may run in between (for a
    // human, up to and including the pane).
    let found = chain
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, row)| ctx.os.client_of(row).map(|client| (index, client)));
    let last = found.map_or(chain.len() - 1, |(index, _)| index - 1);
    if !chain[1..=last].iter().all(|row| ctx.os.is_shell(row)) {
        return Err(caller_refusal(2, "caller runs under a non-shell process"));
    }
    let agent = match found {
        None => None,
        Some((index, _)) => {
            let row = &chain[index];
            let opened = ctx
                .os
                .open(row.pid)
                .ok_or_else(|| caller_refusal(1, "agent start is unreadable"))?;
            // A start other than the listed one is a pid reused after the walk read it.
            if !row
                .start
                .is_some_and(|listed| ctx.os.same_start(opened.start, listed))
            {
                return Err(caller_refusal(1, "agent process changed"));
            }
            Some(Agent {
                pid: row.pid,
                start: opened.start,
                path: row.path.clone(),
            })
        }
    };
    Ok(Caller {
        kind: if agent.is_some() {
            Kind::Agent
        } else {
            Kind::Human
        },
        client: found.map(|(_, client)| client),
        agent,
        chain,
        pane: pane.to_string(),
        window: probe.window.to_string(),
        session: probe.session.to_string(),
        server_pid: endpoint.server_pid,
        server_start,
        socket: endpoint.socket,
        server_key,
        agent_tag: probe.tag.to_string(),
    })
}

/// The caller's own row, then up to 64 parents, one row read each, ending at the pane. A parent that started
/// after its child is a reused pid and ends the walk, as does a missing row or one without a start (it cannot be
/// ordered); a cycle ends at the cap.
fn walk(ctx: &Ctx, pane_pid: u32) -> Res<Vec<Row>> {
    let mut chain = Vec::new();
    let mut next = ctx.os.row(ctx.pid).filter(|row| row.start.is_some());
    while let Some(row) = next {
        let (pid, ppid, start) = (row.pid, row.ppid, row.start);
        chain.push(row);
        if pid == pane_pid {
            return Ok(chain);
        }
        if chain.len() > MAX_PARENTS {
            break;
        }
        // Both starts are known here, so the comparison is between numbers.
        next = ctx
            .os
            .row(ppid)
            .filter(|parent| parent.start.is_some() && parent.start <= start);
    }
    Err(caller_refusal(1, "caller is not below its rmux pane"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::Env;
    use crate::testing::{self, Fakes};

    // The fakes' own pid is the caller.
    const CALLER: u32 = 4242;
    const SHELL: u32 = 170;
    const CLAUDE: u32 = 150;
    const PANE: u32 = 100;
    const SERVER: u32 = 900;
    const SERVER_START: i64 = 123_456_789;

    fn socket() -> &'static str {
        if cfg!(windows) {
            r"\\.\pipe\rmux-me"
        } else {
            "/tmp/rmux-501/main"
        }
    }

    fn human() -> Fakes {
        human_with_probe(0, "900|100|$3|@1|\n")
    }

    // caller 4242 > shell 170 > claude 150 > pane 100.
    fn agent() -> Fakes {
        let fakes = Fakes::default();
        fakes.process(testing::row(SERVER, 1, SERVER_START));
        fakes.process(testing::row(PANE, SERVER, 1000));
        fakes.process(testing::row(CLAUDE, PANE, 1500));
        fakes.process(testing::row(SHELL, CLAUDE, 1700));
        fakes.process(testing::row(CALLER, SHELL, 2000));
        fakes.shell(PANE);
        fakes.shell(SHELL);
        fakes.client(CLAUDE, Client::Claude);
        fakes.mux_rule("display-message", 0, "900|100|$3|@1|tag-7\r\n", "");
        fakes
    }

    fn env(pane: &str) -> Env {
        Env::from_pairs([
            ("TMUX", format!("{},{SERVER},0", socket())),
            ("TMUX_PANE", pane.to_string()),
        ])
    }

    fn run(fakes: &Fakes) -> Res<Caller> {
        let mut ctx = fakes.ctx();
        ctx.env = env("%1");
        resolve(&ctx)
    }

    fn outcome(result: Res<Caller>) -> String {
        match result {
            Ok(caller) => format!("{:?}", caller.kind),
            Err(r) => format!("{} {} {}", r.code, r.check, r.message),
        }
    }

    fn chain(caller: &Caller) -> Vec<u32> {
        caller.chain.iter().map(|row| row.pid).collect()
    }

    #[test]
    fn pane_not_percent_n_refuses_endpoint() {
        for pane in ["", "1", "%", "%1a", "%-1", " %1", "%1\n", "$1"] {
            let fakes = human();
            let mut ctx = fakes.ctx();
            ctx.env = env(pane);
            assert_eq!(
                outcome(resolve(&ctx)),
                "1 endpoint not inside an rmux pane",
                "{pane:?}"
            );
            assert!(fakes.mux_calls().is_empty(), "{pane:?}");
        }
        // No endpoint at all.
        let fakes = human();
        let mut ctx = fakes.ctx();
        ctx.env = Env::from_pairs([("TMUX_PANE", "%1")]);
        assert_eq!(outcome(resolve(&ctx)), "1 endpoint not inside an rmux pane");
    }

    #[test]
    fn probe_bad_row_refuses_endpoint() {
        // rmux 0.10.0 answers a probe of a gone pane with exit 0 and empty fields (Tests.ps1:529-555).
        for row in [
            "",
            "\n",
            "||||\n",
            "900||||\n",
            "900|||@1|\n",
            "900|100|3|@1|",
            "900|100|$|@1|",
            "900|100|$3|1|",
            "900|100|$3|@|",
            "900|x|$3|@1|",
            "900||$3|@1|",
            "|100|$3|@1|",
            "x900|100|$3|@1|",
            "900|100|$3|@1",
            "900|100|$3|@1|\nextra",
            "900|99999999999|$3|@1|",
        ] {
            let fakes = human_with_probe(0, row);
            assert_eq!(
                outcome(run(&fakes)),
                "1 endpoint rmux endpoint did not answer",
                "{row:?}"
            );
        }
        let failed = human_with_probe(1, "900|100|$3|@1|\n");
        assert_eq!(
            outcome(run(&failed)),
            "1 endpoint rmux endpoint did not answer"
        );
        // The probe is the one pinned call, and a tag keeps everything after the fourth bar.
        let fakes = human_with_probe(0, "900|100|$3|@1|a|b\n");
        let caller = run(&fakes).unwrap();
        assert_eq!(caller.agent_tag, "a|b");
        assert_eq!(
            fakes.mux_calls(),
            [vec![
                "-N",
                "display-message",
                "-p",
                "-t",
                "%1",
                "#{pid}|#{pane_pid}|#{session_id}|#{window_id}|#{@agent-id}"
            ]]
        );
    }

    // caller 4242 > shell 170 > pane 100 (a shell), served by 900; the probe of pane %1 answers `stdout`.
    fn human_with_probe(code: i32, stdout: &str) -> Fakes {
        let fakes = Fakes::default();
        fakes.process(testing::row(SERVER, 1, SERVER_START));
        fakes.process(testing::row(PANE, SERVER, 1000));
        fakes.process(testing::row(SHELL, PANE, 1700));
        fakes.process(testing::row(CALLER, SHELL, 2000));
        fakes.shell(PANE);
        fakes.shell(SHELL);
        fakes.mux_rule("display-message", code, stdout, "");
        fakes
    }

    #[test]
    fn server_pid_changed_refuses() {
        let fakes = human_with_probe(0, "901|100|$3|@1|\n");
        assert_eq!(outcome(run(&fakes)), "1 endpoint rmux server changed");
    }

    #[test]
    fn server_start_unreadable_refuses() {
        let fakes = human();
        fakes.opens(SERVER, &[None]);
        assert_eq!(
            outcome(run(&fakes)),
            "1 endpoint rmux server start is unreadable"
        );
    }

    #[test]
    fn server_key_matches_0_1_0_vector() {
        // Independent sha256 (shasum -a 256) of "<socket>|900|123456789".
        let want = if cfg!(windows) {
            "41983d1a5c820c41"
        } else {
            "f939c567531fd0d4"
        };
        let caller = run(&human()).unwrap();
        assert_eq!(caller.server_key, want);
        assert_eq!(
            (
                caller.server_pid,
                caller.server_start,
                caller.socket.as_str()
            ),
            (SERVER, SERVER_START, socket())
        );
    }

    #[test]
    fn not_below_pane_refuses_caller_1() {
        // The pane's row missing, the caller's own row missing, or a pane that is not an ancestor.
        let no_pane = Fakes::default();
        no_pane.process(testing::row(SERVER, 1, SERVER_START));
        no_pane.process(testing::row(SHELL, PANE, 1700));
        no_pane.process(testing::row(CALLER, SHELL, 2000));
        no_pane.shell(SHELL);
        no_pane.mux_rule("display-message", 0, "900|100|$3|@1|\n", "");
        let no_caller = Fakes::default();
        no_caller.process(testing::row(SERVER, 1, SERVER_START));
        no_caller.process(testing::row(PANE, SERVER, 1000));
        no_caller.mux_rule("display-message", 0, "900|100|$3|@1|\n", "");
        let other_pane = human_with_probe(0, "900|50|$3|@1|\n");
        other_pane.process(testing::row(50, 4, 500));
        for fakes in [no_pane, no_caller, other_pane] {
            assert_eq!(
                outcome(run(&fakes)),
                "1 caller caller is not below its rmux pane"
            );
        }
    }

    #[test]
    fn non_shell_between_refuses_caller_2() {
        let not_shell = "2 caller caller runs under a non-shell process";
        // A non-shell between a human caller and its pane, or the pane itself.
        let fakes = Fakes::default();
        fakes.process(testing::row(SERVER, 1, SERVER_START));
        fakes.process(testing::row(PANE, SERVER, 1000));
        fakes.process(testing::row(SHELL, PANE, 1700));
        fakes.process(testing::row(CALLER, SHELL, 2000));
        fakes.shell(PANE);
        fakes.mux_rule("display-message", 0, "900|100|$3|@1|\n", "");
        assert_eq!(outcome(run(&fakes)), not_shell);
        let pane_not_shell = Fakes::default();
        pane_not_shell.process(testing::row(SERVER, 1, SERVER_START));
        pane_not_shell.process(testing::row(PANE, SERVER, 1000));
        pane_not_shell.process(testing::row(SHELL, PANE, 1700));
        pane_not_shell.process(testing::row(CALLER, SHELL, 2000));
        pane_not_shell.shell(SHELL);
        pane_not_shell.mux_rule("display-message", 0, "900|100|$3|@1|\n", "");
        assert_eq!(outcome(run(&pane_not_shell)), not_shell);
        // Between the caller and its agent only shells may run; above the agent anything may.
        let fakes = Fakes::default();
        fakes.process(testing::row(SERVER, 1, SERVER_START));
        fakes.process(testing::row(PANE, SERVER, 1000));
        fakes.process(testing::row(CLAUDE, PANE, 1500));
        fakes.process(testing::row(SHELL, CLAUDE, 1700));
        fakes.process(testing::row(CALLER, SHELL, 2000));
        fakes.client(CLAUDE, Client::Claude);
        fakes.mux_rule("display-message", 0, "900|100|$3|@1|\n", "");
        assert_eq!(outcome(run(&fakes)), not_shell);
        fakes.shell(SHELL);
        let caller = run(&fakes).unwrap();
        assert_eq!(
            (caller.kind, caller.client, chain(&caller)),
            (
                Kind::Agent,
                Some(Client::Claude),
                vec![CALLER, SHELL, CLAUDE, PANE]
            )
        );
        // The caller's own row is neither checked nor an agent.
        let fakes = human();
        fakes.client(CALLER, Client::Copilot);
        assert_eq!(outcome(run(&fakes)), "Human");
    }

    #[test]
    fn walk_stops_at_reused_parent() {
        // A parent that started after its child is a reused pid; one that started in the same tick is the parent.
        let later = Fakes::default();
        later.process(testing::row(SERVER, 1, SERVER_START));
        later.process(testing::row(PANE, SERVER, 1701));
        later.process(testing::row(SHELL, PANE, 1700));
        later.process(testing::row(CALLER, SHELL, 2000));
        later.shell(PANE);
        later.shell(SHELL);
        later.mux_rule("display-message", 0, "900|100|$3|@1|\n", "");
        assert_eq!(
            outcome(run(&later)),
            "1 caller caller is not below its rmux pane"
        );
        let same = Fakes::default();
        same.process(testing::row(SERVER, 1, SERVER_START));
        same.process(testing::row(PANE, SERVER, 1700));
        same.process(testing::row(SHELL, PANE, 1700));
        same.process(testing::row(CALLER, SHELL, 2000));
        same.shell(PANE);
        same.shell(SHELL);
        same.mux_rule("display-message", 0, "900|100|$3|@1|\n", "");
        assert_eq!(outcome(run(&same)), "Human");
    }

    #[test]
    fn walk_unknown_start_is_unreadable() {
        // A row listed without a start cannot be ordered against its child, so it ends the walk like a missing one.
        let shell = human();
        shell.process(testing::unknown_start(SHELL, PANE));
        let own = human();
        own.process(testing::unknown_start(CALLER, SHELL));
        let pane = human();
        pane.process(testing::unknown_start(PANE, SERVER));
        // The fakes answer the first listed row of a pid, so the unknown one goes first.
        for fakes in [shell, own, pane] {
            fakes.prefer_last();
            assert_eq!(
                outcome(run(&fakes)),
                "1 caller caller is not below its rmux pane"
            );
        }
    }

    // caller > `hops` shells > pane.
    fn tall(hops: u32) -> Fakes {
        let fakes = Fakes::default();
        fakes.process(testing::row(SERVER, 1, SERVER_START));
        fakes.process(testing::row(PANE, SERVER, 1000));
        fakes.shell(PANE);
        let mut parent = PANE;
        for index in 1..=hops {
            fakes.process(testing::row(1000 + index, parent, 1000 + i64::from(index)));
            fakes.shell(1000 + index);
            parent = 1000 + index;
        }
        fakes.process(testing::row(CALLER, parent, 5000));
        fakes.mux_rule("display-message", 0, "900|100|$3|@1|\n", "");
        fakes
    }

    #[test]
    fn walk_stops_after_64_parents_refuses_caller_1() {
        // Tests.ps1:593: 64 rows above the caller reach the pane; 65 do not.
        assert_eq!(outcome(run(&tall(63))), "Human");
        let fakes = tall(64);
        assert_eq!(
            outcome(run(&fakes)),
            "1 caller caller is not below its rmux pane"
        );
        // One row per ancestor and never the whole table.
        let calls = fakes.os_calls();
        assert!(!calls.iter().any(|c| c == "table"), "{calls:?}");
        let rows = calls.iter().filter(|c| c.starts_with("row ")).count();
        assert!(rows <= 66, "{rows} row reads");
    }

    #[test]
    fn walk_cycle_terminates_refuses_caller_1() {
        // Tests.ps1:607: two rows that are each other's parent, started in the same tick.
        let fakes = Fakes::default();
        fakes.process(testing::row(SERVER, 1, SERVER_START));
        fakes.process(testing::row(PANE, SERVER, 1000));
        fakes.process(testing::row(300, 301, 2000));
        fakes.process(testing::row(301, 300, 2000));
        fakes.process(testing::row(CALLER, 300, 2000));
        fakes.shell(300);
        fakes.shell(301);
        fakes.mux_rule("display-message", 0, "900|100|$3|@1|\n", "");
        assert_eq!(
            outcome(run(&fakes)),
            "1 caller caller is not below its rmux pane"
        );
    }

    #[test]
    fn agent_start_changed_refuses() {
        let fakes = agent();
        let caller = run(&fakes).unwrap();
        assert_eq!(
            caller.agent,
            Some(Agent {
                pid: CLAUDE,
                start: 1500,
                path: None
            })
        );
        assert_eq!(caller.agent_tag, "tag-7");
        let changed = agent();
        changed.opens(CLAUDE, &[Some(1501)]);
        assert_eq!(outcome(run(&changed)), "1 caller agent process changed");
        let unreadable = agent();
        unreadable.opens(CLAUDE, &[None]);
        assert_eq!(
            outcome(run(&unreadable)),
            "1 caller agent start is unreadable"
        );
    }

    #[test]
    fn human_kind_when_no_client() {
        let fakes = human();
        let caller = run(&fakes).unwrap();
        assert_eq!(
            (caller.kind, caller.client, caller.agent.clone()),
            (Kind::Human, None, None)
        );
        assert_eq!(chain(&caller), [CALLER, SHELL, PANE]);
        assert_eq!(
            (
                caller.pane.as_str(),
                caller.window.as_str(),
                caller.session.as_str(),
                caller.agent_tag.as_str()
            ),
            ("%1", "@1", "$3", "")
        );
        // A caller that is the pane process has a one-row chain.
        let fakes = human_with_probe(0, &format!("900|{CALLER}|$3|@1|\n"));
        let caller = run(&fakes).unwrap();
        assert_eq!((caller.kind, chain(&caller)), (Kind::Human, vec![CALLER]));
    }
}
