#![cfg(feature = "live")]
//! report-2b on the real process table (Linux /proc, macOS proc_pidinfo, Windows Toolhelp): the fake client, copied
//! under a client's name, runs `report -Session` through a shell that stays in the ancestry; the entry's pid and
//! start are what the OS reports; `live::sweep` keeps the entry while the client lives and removes it after.
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod native;

use mux_agent::parse::Client;
use mux_agent::state::live;
use serde_json::Value;

const SID: &str = "0f11fc2f-2032-48ef-a920-4af2a8e5af50";
const EXE: &str = if cfg!(windows) { ".exe" } else { "" };

fn wait_for(p: &Path) -> Value {
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(v) = std::fs::read_to_string(p)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
        {
            return v;
        }
        assert!(Instant::now() < end, "{}", p.display());
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The entry the fake client runs: a shell that runs mux-agent as its child (no `exec`), so a shell sits between.
fn entry(bin: &Path) -> PathBuf {
    let agent = env!("CARGO_BIN_EXE_mux-agent");
    if cfg!(windows) {
        let p = bin.join("entry.cmd");
        std::fs::write(
            &p,
            format!("@echo off\r\n\"{agent}\" %*\r\nexit /b %errorlevel%\r\n"),
        )
        .unwrap();
        p
    } else {
        let p = bin.join("entry");
        std::fs::write(&p, format!("#!/bin/sh\n\"{agent}\" \"$@\"\nexit $?\n")).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        p
    }
}

#[test]
fn the_walk_finds_the_real_client_through_a_shell_and_sweep_follows_its_life() {
    for (name, kind) in [("claude", Client::Claude), ("copilot", Client::Copilot)] {
        let dir = std::env::temp_dir().join(format!("live-session-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let client = bin.join(format!("{name}{EXE}"));
        std::fs::copy(env!("CARGO_BIN_EXE_fake-client"), &client).unwrap();
        let state = dir.join("state");
        native::private_root(&state); // PM call 16: 0700 on Unix before the binary sees it
        let mut cmd = std::process::Command::new(&client);
        for v in [
            "TMUX",
            "RMUX",
            "TMUX_PANE",
            "HC_TERMINAL_ID",
            "SANDBOX_RUNTIME",
        ] {
            cmd.env_remove(v);
        }
        let mut child = cmd
            .env("MUX_IT_DIR", &dir)
            .env("MUX_IT_ENTRY", entry(&bin))
            .env("MUX_AGENTS_STATE", &state)
            .stdin(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        std::fs::write(
            dir.join(format!("cmd.{pid}")),
            format!("report\t-Session\t{SID}\t-Source\tstartup\n"),
        )
        .unwrap();
        let reply = wait_for(&dir.join(format!("reply.{pid}.1.json")));
        let ctx = native::ctx(&state);
        let start = ctx.os.open(pid).expect("the client is open").start;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_eq!(
                (
                    &reply["ok"],
                    &reply["client"]["kind"],
                    &reply["client"]["pid"],
                    &reply["seq"]
                ),
                (
                    &Value::Bool(true),
                    &Value::from(name),
                    &Value::from(pid),
                    &Value::from(1)
                ),
                "{name}: {reply}"
            );
            let got = reply["client"]["start"].as_i64().unwrap();
            assert!(
                ctx.os.same_start(got, start),
                "{name}: the OS start {start}, the entry {got}"
            );
            assert!(live::path(&ctx, kind, pid, got).is_file(), "{name}");
            assert_eq!(
                live::sweep(&ctx).unwrap(),
                0,
                "{name}: kept while the client lives"
            );
            got
        }));
        std::fs::write(dir.join(format!("quit.{pid}")), "0").unwrap();
        child.wait().unwrap();
        drop(child);
        let got = outcome.unwrap();
        // PM call 17: Dead once the pid has left the process table (on Windows a just-exited process may still open
        // by handle); bounded, never assumed at once.
        let end = std::time::Instant::now() + Duration::from_secs(10);
        while live::alive(&ctx, pid, got) != live::Alive::Dead {
            assert!(
                std::time::Instant::now() < end,
                "{name}: still listed 10 s after exit"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(
            live::sweep(&ctx).unwrap(),
            1,
            "{name}: removed after the client exits"
        );
        assert!(!live::path(&ctx, kind, pid, got).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
