//! The only place a child process starts (spec D3): the multiplexer, captured, and the client, attached.

use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
#[cfg(windows)]
use std::sync::mpsc::{self, RecvTimeoutError};
#[cfg(windows)]
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
    let [stdout, stderr] = collect(&mut child, deadline)?;
    let code = exit_code(child.wait()?);
    Ok(Out {
        code,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "mux-agent: native call timed out")
}

/// Both pipes to their end and the exit seen (not reaped), or the group killed and an error. One thread polls
/// both pipes, so a full stderr never stalls a child still writing stdout, and nothing outlives the call: on a
/// timeout the pipes close here even when a process outside the group still holds their other ends.
#[cfg(unix)]
fn collect(child: &mut Child, deadline: Instant) -> io::Result<[Vec<u8>; 2]> {
    use std::os::fd::{AsRawFd, OwnedFd};
    let pipes = [
        child.stdout.take().map(OwnedFd::from),
        child.stderr.take().map(OwnedFd::from),
    ];
    let mut pipes = pipes.map(|p| Some(std::fs::File::from(p.expect("both pipes were requested"))));
    let mut outputs = [Vec::new(), Vec::new()];
    let mut buf = vec![0u8; 64 * 1024];
    let mut step = FIRST_STEP;
    let mut done = false;
    loop {
        if !done {
            done = match exited(child) {
                Ok(done) => done,
                Err(error) => {
                    kill_tree(child);
                    return Err(error);
                }
            };
        }
        if done && pipes.iter().all(Option::is_none) {
            return Ok(outputs);
        }
        let now = Instant::now();
        if now >= deadline {
            kill_tree(child);
            return Err(timed_out());
        }
        // The exit cannot be polled, so the wait is short and grows; output wakes it at once.
        let wait = step.min(deadline - now).max(Duration::from_millis(1));
        // The open pipes' indices, and a pollfd for each in the same order.
        let open: Vec<usize> = (0..2).filter(|&i| pipes[i].is_some()).collect();
        let mut fds: Vec<libc::pollfd> = open
            .iter()
            .filter_map(|&i| pipes[i].as_ref())
            .map(|pipe| libc::pollfd {
                fd: pipe.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        // SAFETY: `fds` holds `fds.len()` live pollfds; with none, poll only sleeps.
        let ready = unsafe {
            libc::poll(
                fds.as_mut_ptr(),
                fds.len() as libc::nfds_t,
                wait.as_millis() as libc::c_int,
            )
        };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            kill_tree(child);
            return Err(error);
        }
        if ready == 0 {
            step = (step * 2).min(MAX_STEP);
            continue;
        }
        for (&index, item) in open.iter().zip(&fds) {
            if item.revents == 0 {
                continue;
            }
            let Some(pipe) = pipes[index].as_mut() else {
                continue;
            };
            // Ready, so one read does not block: data, or 0 at the end once every writer closed.
            match pipe.read(&mut buf) {
                Ok(0) => pipes[index] = None,
                Ok(n) => outputs[index].extend_from_slice(&buf[..n]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    kill_tree(child);
                    return Err(error);
                }
            }
        }
    }
}

/// Windows pipes cannot be polled, so a thread reads each. Each thread sends its pipe back with its bytes, so the
/// handle stays open until the thread is joined; on a timeout CancelIoEx aborts the pending read (std reads child
/// pipes with ReadFileEx and an alertable wait), so no thread or handle outlives the call.
#[cfg(windows)]
fn collect(child: &mut Child, deadline: Instant) -> io::Result<[Vec<u8>; 2]> {
    use std::os::windows::io::AsRawHandle;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    type Pipe = Box<dyn Read + Send>;
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<(usize, Vec<u8>, Pipe)>();
    let out = child.stdout.take().expect("both pipes were requested");
    let err = child.stderr.take().expect("both pipes were requested");
    let handles = [out.as_raw_handle() as isize, err.as_raw_handle() as isize];
    let pipes: [Pipe; 2] = [Box::new(out), Box::new(err)];
    let readers: Vec<thread::JoinHandle<()>> = pipes
        .into_iter()
        .enumerate()
        .map(|(index, mut pipe)| {
            let (tx, stop) = (tx.clone(), stop.clone());
            thread::spawn(move || {
                let mut bytes = Vec::new();
                let mut buf = vec![0u8; 64 * 1024];
                // Any error ends the read, so a cancelled one cannot be retried forever.
                while !stop.load(Ordering::SeqCst) {
                    match pipe.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => bytes.extend_from_slice(&buf[..n]),
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
                let _ = tx.send((index, bytes, pipe));
            })
        })
        .collect();
    drop(tx);

    let mut outputs: [Option<Vec<u8>>; 2] = [None, None];
    // Held until the readers are joined, so their handles are never closed under a cancel.
    let mut returned: Vec<Pipe> = Vec::new();
    let mut done = false;
    let mut step = FIRST_STEP;
    let result = loop {
        if !done {
            done = match exited(child) {
                Ok(done) => done,
                Err(error) => {
                    kill_tree(child);
                    break Err(error);
                }
            };
        }
        if done && outputs.iter().all(Option::is_some) {
            break Ok(outputs.clone().map(Option::unwrap_or_default));
        }
        let now = Instant::now();
        if now >= deadline {
            kill_tree(child);
            break Err(timed_out());
        }
        let wait = step.min(deadline - now);
        match rx.recv_timeout(wait) {
            Ok((index, bytes, pipe)) => {
                outputs[index] = Some(bytes);
                returned.push(pipe);
            }
            Err(RecvTimeoutError::Timeout) => step = (step * 2).min(MAX_STEP),
            Err(RecvTimeoutError::Disconnected) => {
                thread::sleep(wait);
                step = (step * 2).min(MAX_STEP);
            }
        }
    };
    if result.is_err() {
        stop.store(true, Ordering::SeqCst);
        // Repeated, since a read may start between the flag and a cancel; bounded, since the flag ends it.
        for _ in 0..CANCEL_ROUNDS {
            if readers.iter().all(thread::JoinHandle::is_finished) {
                break;
            }
            for (index, reader) in readers.iter().enumerate() {
                if !reader.is_finished() {
                    // SAFETY: the pipe is alive (owned by the reader or in `rx`/`returned` until the join below);
                    // a null OVERLAPPED cancels every read on it.
                    unsafe { CancelIoEx(handles[index] as _, std::ptr::null()) };
                }
            }
            thread::sleep(MAX_STEP / 10);
        }
    }
    for reader in readers {
        let _ = reader.join();
    }
    drop(returned);
    drop(rx);
    result
}

#[cfg(windows)]
const CANCEL_ROUNDS: u32 = 200;

// kernel32; windows-sys has it only under Win32_System_IO, which nothing else here needs.
#[cfg(windows)]
unsafe extern "system" {
    fn CancelIoEx(file: *mut core::ffi::c_void, overlapped: *const core::ffi::c_void) -> i32;
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
    fn captured_drains_both_pipes_past_their_buffers() {
        // 1 MiB to each stream, stderr first: far past a pipe's buffer, so it ends only if both are read as they fill.
        let script =
            "head -c 1048576 /dev/zero | tr '\\0' e >&2; head -c 1048576 /dev/zero | tr '\\0' o";
        let out = sh(script, Duration::from_secs(10)).unwrap();
        assert_eq!(out.code, 0);
        assert_eq!((out.stdout.len(), out.stderr.len()), (1 << 20, 1 << 20));
        assert!(out.stdout.bytes().all(|b| b == b'o'));
        assert!(out.stderr.bytes().all(|b| b == b'e'));
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

    fn open_fds() -> usize {
        // /dev/fd lists this process's descriptors on Linux and macOS; reading it opens one more, every time.
        std::fs::read_dir("/dev/fd").unwrap().count()
    }

    #[cfg(target_os = "macos")]
    fn threads() -> usize {
        let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
        let size = size_of::<libc::proc_taskinfo>() as libc::c_int;
        // SAFETY: the buffer is one proc_taskinfo of `size` bytes.
        let got = unsafe {
            libc::proc_pidinfo(
                libc::getpid(),
                libc::PROC_PIDTASKINFO,
                0,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        assert_eq!(got, size);
        // SAFETY: zeroed is valid and the call filled it.
        unsafe { info.assume_init() }.pti_threadnum as usize
    }

    #[cfg(target_os = "linux")]
    fn threads() -> usize {
        std::fs::read_dir("/proc/self/task").unwrap().count()
    }

    #[test]
    fn timeout_leaves_no_reader_or_descriptor_behind() {
        // In a process of its own, so other tests' threads and descriptors do not move the counts.
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "spawn::tests::timeout_leak_child",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stdout.contains("NO-LEAK"), "{stdout}\n{stderr}");
    }

    #[test]
    #[ignore = "a child of timeout_leaves_no_reader_or_descriptor_behind"]
    fn timeout_leak_child() {
        // `set -m` puts the sleeper in a group of its own, so the timeout's killpg misses it and it keeps both
        // pipes open for 30 s.
        let dir = TempDir::new();
        let pids = dir.path().join("pids");
        let script = format!("set -m; sleep 30 & echo $! >> '{}'; wait", pids.display());
        let baseline = (open_fds(), threads());
        let mut counts = Vec::new();
        for _ in 0..8 {
            let error = sh(&script, Duration::from_millis(200)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            counts.push((open_fds(), threads()));
        }
        for pid in std::fs::read_to_string(&pids).unwrap().split_whitespace() {
            // SAFETY: SIGKILL to the sleepers this test started.
            unsafe { libc::kill(pid.parse().unwrap(), libc::SIGKILL) };
        }
        assert_eq!(counts, vec![baseline; 8]);
        println!("NO-LEAK");
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
