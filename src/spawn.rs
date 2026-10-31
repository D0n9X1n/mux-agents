//! The only place a child process starts (spec D3): the multiplexer, captured, and the client, attached.

use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

/// One finished captured call; output is UTF-8, invalid bytes replaced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

// Short first waits keep a fast call fast; the cap bounds the polling cost of a slow one.
const FIRST_STEP: Duration = Duration::from_millis(1);
const MAX_STEP: Duration = Duration::from_millis(50);

/// One bounded call (MuxAgents.ps1:262-288): stdin closed, output read to the end, working directory home. The
/// call is done once the process exited and both pipes closed; past `timeout` its process group (Unix) or the
/// process (Windows) is killed and the call fails with `TimedOut`.
pub fn captured(path: &Path, args: &[&str], timeout: Duration) -> io::Result<Out> {
    audit(path)?;
    let mut command = Command::new(path);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(home) = std::env::home_dir() {
        command.current_dir(home);
    }
    own_group(&mut command);
    let mut child = command.spawn()?;
    let deadline = Instant::now() + timeout;

    // Reader threads, so a full stderr pipe never stalls a child that is still writing stdout.
    let (tx, rx) = mpsc::channel();
    let pipes: [Option<Box<dyn Read + Send>>; 2] = [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    ];
    for (index, pipe) in pipes.into_iter().enumerate() {
        let tx = tx.clone();
        let mut pipe = pipe.expect("both pipes were requested");
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            let _ = tx.send((index, bytes));
        });
    }
    drop(tx);

    let mut outputs: [Option<Vec<u8>>; 2] = [None, None];
    let mut done = false;
    let mut step = FIRST_STEP;
    loop {
        if !done {
            done = match exited(&mut child) {
                Ok(done) => done,
                Err(error) => {
                    kill_tree(&mut child);
                    return Err(error);
                }
            };
        }
        if done && outputs.iter().all(Option::is_some) {
            let code = exit_code(child.wait()?);
            let [stdout, stderr] =
                outputs.map(|o| String::from_utf8_lossy(&o.unwrap_or_default()).into_owned());
            return Ok(Out {
                code,
                stdout,
                stderr,
            });
        }
        let now = Instant::now();
        if now >= deadline {
            // The readers are left behind: a pipe a killed process held closes, one a stray process holds never does.
            kill_tree(&mut child);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "mux-agent: native call timed out",
            ));
        }
        let wait = step.min(deadline - now);
        match rx.recv_timeout(wait) {
            Ok((index, bytes)) => outputs[index] = Some(bytes),
            Err(RecvTimeoutError::Timeout) => step = (step * 2).min(MAX_STEP),
            // Both pipes are read; only the exit is left to see.
            Err(RecvTimeoutError::Disconnected) => {
                thread::sleep(wait);
                step = (step * 2).min(MAX_STEP);
            }
        }
    }
}

/// The client on the caller's own console (MuxAgents.ps1:1650-1659): stdio inherited, `remove` taken out of its
/// environment; its exit code.
pub fn attached(path: &Path, args: &[String], remove: &[&str]) -> io::Result<i32> {
    audit(path)?;
    let mut command = Command::new(path);
    command.args(args);
    for name in remove {
        command.env_remove(name);
    }
    Ok(exit_code(command.status()?))
}

#[cfg(unix)]
fn own_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // Its own group, so a timeout reaches what it started (the reference killed the tree).
    command.process_group(0);
}

#[cfg(windows)]
fn own_group(_: &mut Command) {}

/// Whether the child exited, without reaping it: until `wait`, its pid and group id cannot be reused, so a kill
/// after an exit still reaches only what it started.
#[cfg(unix)]
fn exited(child: &mut Child) -> io::Result<bool> {
    loop {
        // SAFETY: an all-zero siginfo_t is valid; waitid only writes into it.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: the pid is our unreaped child and `info` is a live siginfo_t.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                child.id() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            // WNOHANG with nothing to report leaves the zeroed info as it was.
            return Ok(info.si_signo != 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

// The open process handle keeps the pid from reuse until the child is dropped.
#[cfg(windows)]
fn exited(child: &mut Child) -> io::Result<bool> {
    Ok(child.try_wait()?.is_some())
}

#[cfg(unix)]
fn kill_tree(child: &mut Child) {
    // SAFETY: the group is the unreaped child's own, so its id cannot name anyone else's group yet.
    unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
    let _ = child.wait();
}

// Only the child: its descendants would need a job object assigned before it runs.
#[cfg(windows)]
fn kill_tree(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    // A signal leaves as 128 + signal, as .NET's Process.ExitCode reported it.
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

#[cfg(windows)]
fn exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

// The live tests count every process mux-agent starts.
fn audit(path: &Path) -> io::Result<()> {
    if cfg!(feature = "live")
        && let Some(dir) = std::env::var_os("MUX_IT_DIR")
    {
        audit_to(Path::new(&dir), path)?;
    }
    Ok(())
}

fn audit_to(dir: &Path, path: &Path) -> io::Result<()> {
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("spawned.log"))?;
    // One write, so concurrent callers' lines do not interleave.
    log.write_all(format!("{}\n", path.display()).as_bytes())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::testing::TempDir;
    use std::time::Instant;

    const SH: &str = "/bin/sh";

    fn sh(script: &str, timeout: Duration) -> io::Result<Out> {
        captured(Path::new(SH), &["-c", script], timeout)
    }

    #[test]
    fn captured_closes_stdin_and_reads_utf8() {
        // `cat` returns at once only when stdin is closed; the output keeps non-ASCII text and both streams.
        let out = sh(
            "cat; printf 'h\\303\\251llo \\344\\270\\255'; printf 'err\\377' >&2; exit 3",
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(out.code, 3);
        assert_eq!(out.stdout, "héllo 中");
        assert_eq!(out.stderr, "err\u{fffd}");
    }

    #[test]
    fn captured_runs_in_home() {
        let out = sh("pwd -P", Duration::from_secs(5)).unwrap();
        let home = std::fs::canonicalize(std::env::home_dir().unwrap()).unwrap();
        assert_eq!(out.stdout.trim_end(), home.to_str().unwrap());
    }

    #[test]
    fn captured_timeout_kills_and_errs() {
        // The grandchild proves the whole group is killed, not just the direct child.
        let dir = TempDir::new();
        let file = dir.path().join("grandchild");
        let script = format!("sleep 30 & echo $! > '{}'; wait", file.display());
        let started = Instant::now();
        let error = sh(&script, Duration::from_millis(300)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        let pid: i32 = std::fs::read_to_string(&file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let gone = (0..50).any(|_| {
            // SAFETY: kill with signal 0 only probes the pid.
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            if alive {
                std::thread::sleep(Duration::from_millis(50));
            }
            !alive
        });
        assert!(gone, "grandchild {pid} survived");
    }

    #[test]
    fn attached_inherits_stdio_and_removes_env() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "spawn::tests::attached_child",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("MUX_SPAWN_GONE", "x")
            .env("MUX_SPAWN_KEPT", "y")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stdout.contains("ATTACHED gone=[] kept=[y]"), "{stdout}");
        assert!(stderr.contains("ATTACHED-ERR"), "{stderr}");
        assert!(stdout.contains("ATTACHED-CODE 0"), "{stdout}");
    }

    #[test]
    #[ignore = "a child of attached_inherits_stdio_and_removes_env"]
    fn attached_child() {
        let script = "echo \"ATTACHED gone=[$MUX_SPAWN_GONE] kept=[$MUX_SPAWN_KEPT]\"; echo ATTACHED-ERR >&2";
        let code = attached(
            Path::new(SH),
            &["-c".into(), script.into()],
            &["MUX_SPAWN_GONE"],
        )
        .unwrap();
        println!("ATTACHED-CODE {code}");
    }

    #[test]
    fn attached_returns_exit_code() {
        let run = |script: &str| attached(Path::new(SH), &["-c".into(), script.into()], &[]);
        assert_eq!(run("exit 7").unwrap(), 7);
        // A signal leaves as 128 + signal, the code .NET's Process.ExitCode gave the reference.
        assert_eq!(run("kill -9 $$").unwrap(), 137);
        assert!(attached(Path::new("/nonexistent/mux-agent-x"), &[], &[]).is_err());
    }

    #[cfg(feature = "live")]
    #[test]
    fn live_audit_appends_program_path() {
        let dir = TempDir::new();
        audit_to(dir.path(), Path::new(SH)).unwrap();
        let log = std::fs::read_to_string(dir.path().join("spawned.log")).unwrap();
        assert_eq!(log, format!("{SH}\n"));
    }
}
