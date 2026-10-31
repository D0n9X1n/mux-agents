//! The Windows process table: Toolhelp, process handles and the caller's token, no helper processes
//! (MuxAgents.ps1:171-229, 350-355, 836-910). The decisions are pure and compile everywhere; the calls are Windows-only.

use serde_json::{Map, Value};

use super::{Left, Reason, Token};

/// A FILETIME's two halves as the one 100 ns count the server key hashes.
pub fn filetime(high: u32, low: u32) -> i64 {
    ((u64::from(high) << 32) | u64::from(low)) as i64
}

/// Opened and listed starts both come from GetProcessTimes, so they compare exactly.
pub fn same_start(opened: i64, listed: i64) -> bool {
    opened == listed
}

/// C1 on Windows: only a readable token that is neither AppContainer nor restricted, at Medium integrity (8192)
/// or above, is unsandboxed (MuxAgents.ps1:903-910).
pub fn token_admits(token: Option<Token>) -> bool {
    token.is_some_and(|t| !t.app_container && !t.restricted && t.integrity_rid >= 8192)
}

/// Windows counts only the failed kills as survivors (MuxAgents.ps1:218).
pub fn stop_report(left: &[Left]) -> Map<String, Value> {
    let failed = left.iter().filter(|l| l.reason == Reason::Kill).count();
    let mut report = Map::new();
    report.insert("survivors".into(), failed.into());
    report
}

/// The command line from a ProcessCommandLineInformation answer: a UNICODE_STRING at the start of `bytes`, whose
/// Buffer points into `bytes` (at address `base`). A length that is odd or a Buffer outside the answer is none.
pub fn command_line(bytes: &[u8], base: usize) -> Option<Vec<u16>> {
    // Length, MaximumLength, padding to a pointer, then Buffer.
    let word = size_of::<usize>();
    let header = 2 * word;
    let length = usize::from(u16::from_ne_bytes(bytes.get(..2)?.try_into().ok()?));
    let buffer = usize::from_ne_bytes(bytes.get(word..header)?.try_into().ok()?);
    if length % 2 != 0 {
        return None;
    }
    if length == 0 {
        return Some(Vec::new());
    }
    let at = buffer.checked_sub(base).filter(|&at| at >= header)?;
    let text = bytes.get(at..at.checked_add(length)?)?;
    Some(
        text.chunks_exact(2)
            .map(|pair| u16::from_ne_bytes([pair[0], pair[1]]))
            .collect(),
    )
}

#[cfg(windows)]
pub use win::Windows;

#[cfg(windows)]
mod win {
    use std::io;
    use std::mem::{MaybeUninit, size_of};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

    use serde_json::{Map, Value};
    use windows_sys::Wdk::System::Threading::{
        NtQueryInformationProcess, ProcessBasicInformation, ProcessCommandLineInformation,
    };
    use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, FILETIME, HANDLE, WAIT_OBJECT_0};
    use windows_sys::Win32::Security::{
        GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_MANDATORY_LABEL,
        TOKEN_QUERY, TokenIntegrityLevel, TokenIsAppContainer, TokenIsRestricted,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessTimes, OpenProcess, OpenProcessToken, PROCESS_ACCESS_RIGHTS,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
    };

    use super::super::{Left, Opened, Os, Row, Signals, Token};
    use crate::parse::{self, Client};

    pub struct Windows;

    // Reads need only this, so another user's process still lists; a kill also needs to wait and terminate.
    const READ: PROCESS_ACCESS_RIGHTS = PROCESS_QUERY_LIMITED_INFORMATION;
    const HOLD: PROCESS_ACCESS_RIGHTS = READ | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE;
    const KILL_WAIT_MS: u32 = 5000;
    // Long paths reach 32767 units.
    const PATH_UNITS: usize = 32768;

    fn process(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> Option<OwnedHandle> {
        // SAFETY: plain open; null on failure.
        let raw = unsafe { OpenProcess(access, 0, pid) };
        // SAFETY: a non-null result is a fresh handle nothing else owns.
        (!raw.is_null()).then(|| unsafe { OwnedHandle::from_raw_handle(raw) })
    }

    fn raw(handle: &OwnedHandle) -> HANDLE {
        handle.as_raw_handle()
    }

    // The creation FILETIME, 100 ns since 1601: the unit the server key hashes.
    fn start_of(handle: &OwnedHandle) -> Option<i64> {
        let mut times = [FILETIME::default(); 4];
        let [created, exited, kernel, user] = &mut times;
        // SAFETY: four valid out pointers.
        let ok = unsafe { GetProcessTimes(raw(handle), created, exited, kernel, user) };
        (ok != 0).then(|| super::filetime(times[0].dwHighDateTime, times[0].dwLowDateTime))
    }

    fn path_of(handle: &OwnedHandle) -> Option<String> {
        let mut buf = vec![0u16; PATH_UNITS];
        let mut len = buf.len() as u32;
        // SAFETY: `len` units are writable; it comes back as the units written, no NUL.
        let ok = unsafe {
            QueryFullProcessImageNameW(raw(handle), PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len)
        };
        if ok == 0 {
            return None;
        }
        String::from_utf16(buf.get(..len as usize)?).ok()
    }

    // ProcessCommandLineInformation answers a UNICODE_STRING and its text in one buffer.
    fn argv_of(handle: &OwnedHandle) -> Option<Vec<String>> {
        let mut need = 0u32;
        // SAFETY: a zero-length query only reports the size.
        unsafe {
            NtQueryInformationProcess(
                raw(handle),
                ProcessCommandLineInformation,
                std::ptr::null_mut(),
                0,
                &mut need,
            )
        };
        // usize-aligned, as the UNICODE_STRING at its start needs.
        let words = (need as usize).div_ceil(size_of::<usize>()).max(2);
        let mut buf = vec![0usize; words];
        let bytes = (words * size_of::<usize>()) as u32;
        let mut got = 0u32;
        // SAFETY: the buffer holds `bytes` writable bytes.
        let status = unsafe {
            NtQueryInformationProcess(
                raw(handle),
                ProcessCommandLineInformation,
                buf.as_mut_ptr().cast(),
                bytes,
                &mut got,
            )
        };
        if status < 0 {
            return None;
        }
        // SAFETY: the words are initialized bytes; read only as far as the call wrote.
        let answer = unsafe {
            std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), (got.min(bytes)) as usize)
        };
        let line = super::command_line(answer, buf.as_ptr() as usize)?;
        Some(parse::windows_argv(&String::from_utf16_lossy(&line)))
    }

    // <winternl.h>'s PROCESS_BASIC_INFORMATION; windows-sys gates it behind a feature this crate does not need
    // otherwise.
    #[repr(C)]
    struct BasicInformation {
        exit_status: i32,
        peb: usize,
        affinity: usize,
        priority: i32,
        pid: usize,
        ppid: usize,
    }

    // The parent in one call, so a single row needs no snapshot of every process.
    fn ppid_of(handle: &OwnedHandle) -> Option<u32> {
        let mut info = MaybeUninit::<BasicInformation>::zeroed();
        let size = size_of::<BasicInformation>() as u32;
        let mut got = 0u32;
        // SAFETY: the buffer is one BasicInformation of `size` bytes.
        let status = unsafe {
            NtQueryInformationProcess(
                raw(handle),
                ProcessBasicInformation,
                info.as_mut_ptr().cast(),
                size,
                &mut got,
            )
        };
        if status < 0 || got != size {
            return None;
        }
        // SAFETY: zeroed is valid and the call filled it.
        u32::try_from(unsafe { info.assume_init() }.ppid).ok()
    }

    // Every field through one handle, which pins the process, so a reused pid cannot mix two processes. One this
    // user cannot open (Idle, System, protected) has no readable start, so no row.
    fn row_of(pid: u32) -> Option<Row> {
        let handle = process(pid, READ)?;
        Some(Row {
            pid,
            ppid: ppid_of(&handle)?,
            start: start_of(&handle)?,
            path: path_of(&handle),
            argv: argv_of(&handle),
        })
    }

    fn token_handle() -> Option<OwnedHandle> {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: the pseudo handle needs no closing; `token` is a valid out pointer.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return None;
        }
        // SAFETY: a fresh token handle nothing else owns.
        Some(unsafe { OwnedHandle::from_raw_handle(token) })
    }

    fn token_dword(token: &OwnedHandle, class: i32) -> Option<u32> {
        let mut value = 0u32;
        let mut len = 0u32;
        // SAFETY: one u32 out buffer of 4 bytes.
        let ok =
            unsafe { GetTokenInformation(raw(token), class, (&raw mut value).cast(), 4, &mut len) };
        (ok != 0).then_some(value)
    }

    // The mandatory label's last subauthority: 4096 low, 8192 medium, 12288 high.
    fn integrity_rid(token: &OwnedHandle) -> Option<u32> {
        let mut need = 0u32;
        // SAFETY: a zero-length query only reports the size.
        unsafe {
            GetTokenInformation(
                raw(token),
                TokenIntegrityLevel,
                std::ptr::null_mut(),
                0,
                &mut need,
            )
        };
        let mut buf = vec![0u64; (need as usize).div_ceil(8).max(1)];
        let mut len = 0u32;
        // SAFETY: the buffer is 8-byte aligned and its length is passed.
        let ok = unsafe {
            GetTokenInformation(
                raw(token),
                TokenIntegrityLevel,
                buf.as_mut_ptr().cast(),
                (buf.len() * 8) as u32,
                &mut len,
            )
        };
        if ok == 0 {
            return None;
        }
        // SAFETY: the call filled a TOKEN_MANDATORY_LABEL at the start of `buf`; its SID points into `buf`.
        unsafe {
            let sid = (*buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>()).Label.Sid;
            let count = GetSidSubAuthorityCount(sid);
            if count.is_null() || *count == 0 {
                return None;
            }
            let rid = GetSidSubAuthority(sid, u32::from(*count - 1));
            (!rid.is_null()).then(|| *rid)
        }
    }

    impl Os for Windows {
        fn row(&self, pid: u32) -> Option<Row> {
            row_of(pid)
        }

        /// One Toolhelp snapshot names every pid; each row is read through its own handle, and a process that
        /// cannot be opened is skipped.
        fn table(&self) -> io::Result<Vec<Row>> {
            // SAFETY: a process snapshot; INVALID_HANDLE_VALUE on failure.
            let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
            if snap.is_null() || snap as isize == -1 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: a fresh snapshot handle nothing else owns.
            let snap = unsafe { OwnedHandle::from_raw_handle(snap) };
            // SAFETY: zeroed is a valid PROCESSENTRY32W.
            let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
            entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
            let mut rows = Vec::new();
            // SAFETY: a live snapshot and one entry with its size set.
            let mut more = unsafe { Process32FirstW(raw(&snap), &mut entry) } != 0;
            while more {
                if let Some(row) = row_of(entry.th32ProcessID) {
                    rows.push(row);
                }
                // SAFETY: as above.
                more = unsafe { Process32NextW(raw(&snap), &mut entry) } != 0;
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
                return Err(error);
            }
            Ok(rows)
        }

        /// The handle first, then the start through it, so both name the process the handle pins. Without a handle
        /// that may terminate there is nothing to kill with, so the process is not opened (stop leaves it `open`).
        fn open(&self, pid: u32) -> Option<Opened> {
            let handle = process(pid, HOLD)?;
            let start = start_of(&handle)?;
            Some(Opened::new(pid, start, Some(handle)))
        }

        /// True when the opened process exited within 5 s. A failed TerminateProcess is no answer: a process
        /// that already exited refuses it, and the wait still shows the exit (Tests.ps1:166-169).
        fn kill(&self, opened: Opened) -> bool {
            let Some(handle) = opened.handle() else {
                return false;
            };
            // SAFETY: a live process handle; 1 is the exit code it leaves with.
            let _ = unsafe { TerminateProcess(raw(handle), 1) };
            // SAFETY: a live process handle opened with SYNCHRONIZE.
            unsafe { WaitForSingleObject(raw(handle), KILL_WAIT_MS) == WAIT_OBJECT_0 }
        }

        // Windows has no supported read of another process's cwd; C5 reads Claude's session file instead.
        fn cwd(&self, _: u32) -> Option<String> {
            None
        }

        fn client_of(&self, row: &Row) -> Option<Client> {
            parse::windows_client(row.path.as_deref()?)
        }

        fn is_shell(&self, row: &Row) -> bool {
            row.path.as_deref().is_some_and(parse::windows_is_shell)
        }

        fn same_start(&self, opened: i64, listed: i64) -> bool {
            super::same_start(opened, listed)
        }

        fn stop_report(&self, left: &[Left]) -> Map<String, Value> {
            super::stop_report(left)
        }

        /// The caller's own token; none when any part is unreadable.
        fn token(&self) -> Option<Token> {
            let token = token_handle()?;
            Some(Token {
                app_container: token_dword(&token, TokenIsAppContainer)? != 0,
                restricted: token_dword(&token, TokenIsRestricted)? != 0,
                integrity_rid: integrity_rid(&token)?,
            })
        }

        fn sandbox(&self) -> Signals {
            Signals::Token(self.token())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        use std::process::{Child, Command, Stdio};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        };

        fn me() -> u32 {
            std::process::id()
        }

        // About a minute of nothing, with no console or helper of its own.
        fn sleeper() -> Child {
            Command::new("ping")
                .args(["-n", "60", "127.0.0.1"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        }

        fn image(path: &std::path::Path) -> String {
            let full = std::fs::canonicalize(path).unwrap();
            let text = full.to_str().unwrap();
            text.strip_prefix(r"\\?\").unwrap_or(text).to_string()
        }

        #[test]
        fn row_of_self() {
            let row = Windows.row(me()).unwrap();
            assert_eq!(row.pid, me());
            let exe = image(&std::env::current_exe().unwrap());
            assert!(
                row.path.as_deref().unwrap().eq_ignore_ascii_case(&exe),
                "{:?} {exe}",
                row.path
            );
            // The table agrees with the one-row read, the parent is listed, and open reads the same start.
            let table = Windows.table().unwrap();
            let listed = table.iter().find(|r| r.pid == me()).unwrap();
            assert_eq!((listed.ppid, listed.start), (row.ppid, row.start));
            assert!(table.iter().any(|r| r.pid == row.ppid));
            assert_eq!(Windows.open(me()).map(|o| o.start), Some(row.start));
            assert_eq!(Windows.row(0), None);
            assert!(Windows.open(0).is_none());
            assert_eq!(Windows.cwd(me()), None);
        }

        #[test]
        fn row_of_child_names_this_process_as_parent() {
            // The ppid read independently of the table: a child this process started.
            let mut child = sleeper();
            let row = Windows.row(child.id());
            child.kill().unwrap();
            child.wait().unwrap();
            let row = row.unwrap();
            assert_eq!((row.pid, row.ppid), (child.id(), me()));
        }

        #[test]
        fn argv_of_self() {
            let argv = Windows.row(me()).unwrap().argv.unwrap();
            assert_eq!(argv, std::env::args().collect::<Vec<_>>());
        }

        #[test]
        fn open_kill_real_child_terminates() {
            let mut child = sleeper();
            let opened = Windows.open(child.id()).unwrap();
            let listed = Windows.table().unwrap();
            let listed = listed.iter().find(|r| r.pid == child.id()).unwrap();
            assert!(Windows.same_start(opened.start, listed.start));
            assert!(Windows.kill(opened));
            assert!(child.try_wait().unwrap().is_some());
        }

        #[test]
        fn kill_failure_reported() {
            // A handle without PROCESS_TERMINATE: the kill is refused and the child outlives the 5 s wait.
            let mut child = sleeper();
            // SAFETY: plain open; the handle is owned below and closed once.
            let raw = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                    0,
                    child.id(),
                )
            };
            assert!(!raw.is_null());
            // SAFETY: a fresh process handle nothing else owns.
            let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
            let start = Windows.open(child.id()).unwrap().start;
            assert!(!Windows.kill(Opened::new(child.id(), start, Some(handle))));
            assert!(child.try_wait().unwrap().is_none());
            child.kill().unwrap();
            child.wait().unwrap();
        }

        #[test]
        fn already_exited_counts_as_exited() {
            // Exited before the kill: TerminateProcess fails, yet the wait shows the exit (Tests.ps1:166-169).
            let mut child = sleeper();
            let opened = Windows.open(child.id()).unwrap();
            child.kill().unwrap();
            child.wait().unwrap();
            assert!(Windows.kill(opened));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_start_unaligned_filetime_matches() {
        // Not a whole microsecond: the reference rounded the opened side; both sides are exact here.
        let start = 133_000_000_000_000_007;
        assert_eq!(filetime((start >> 32) as u32, start as u32), start);
        assert!(same_start(start, start));
    }

    #[test]
    fn same_start_differs_by_one_tick_false() {
        let start = 133_000_000_000_000_007;
        assert!(!same_start(start, start - 1));
        assert!(!same_start(start, start + 1));
        // Same microsecond, other tick: the reference's rounding would have called these equal.
        assert!(!same_start(
            133_000_000_000_000_000,
            133_000_000_000_000_009
        ));
    }

    #[test]
    fn token_decision() {
        let token = |app_container, restricted, integrity_rid| {
            Some(Token {
                app_container,
                restricted,
                integrity_rid,
            })
        };
        assert!(!token_admits(None));
        assert!(!token_admits(token(true, false, 8192)));
        assert!(!token_admits(token(false, true, 8192)));
        assert!(!token_admits(token(false, false, 4096)));
        assert!(!token_admits(token(false, false, 8191)));
        assert!(!token_admits(token(false, false, 0)));
        assert!(token_admits(token(false, false, 8192)));
        assert!(token_admits(token(false, false, 8448)));
        assert!(token_admits(token(false, false, 12288)));
        assert!(token_admits(token(false, false, 16384)));
    }

    #[test]
    fn stop_report_counts_failed_kills() {
        let left = [
            (310, Reason::Kill),
            (320, Reason::Start),
            (330, Reason::Open),
            (340, Reason::Kill),
        ]
        .map(|(pid, reason)| Left { pid, reason });
        assert_eq!(
            Value::Object(stop_report(&left)).to_string(),
            r#"{"survivors":2}"#
        );
        assert_eq!(
            Value::Object(stop_report(&[])).to_string(),
            r#"{"survivors":0}"#
        );
    }

    // A UNICODE_STRING {Length, MaximumLength, Buffer} then the text, as the kernel lays it out.
    fn answer(base: usize, length: u16, at: usize, text: &[u16]) -> Vec<u8> {
        let word = size_of::<usize>();
        let mut bytes = vec![0u8; 2 * word];
        bytes[..2].copy_from_slice(&length.to_ne_bytes());
        bytes[2..4].copy_from_slice(&length.to_ne_bytes());
        bytes[word..2 * word].copy_from_slice(&(base + at).to_ne_bytes());
        bytes.resize(at, 0);
        bytes.extend(text.iter().flat_map(|u| u.to_ne_bytes()));
        bytes
    }

    #[test]
    fn command_line_inside_buffer() {
        let word = size_of::<usize>();
        let text: Vec<u16> = "a.exe \"b c\"".encode_utf16().collect();
        let len = (text.len() * 2) as u16;
        let base = 0x1000;
        let bytes = answer(base, len, 2 * word, &text);
        assert_eq!(command_line(&bytes, base), Some(text.clone()));
        // Empty: no Buffer needed.
        assert_eq!(
            command_line(&answer(base, 0, 2 * word, &[]), base),
            Some(vec![])
        );
        // Odd length, a Buffer before the text area or past the end, a short answer.
        assert_eq!(
            command_line(&answer(base, len - 1, 2 * word, &text), base),
            None
        );
        assert_eq!(command_line(&bytes, base + 1), None);
        assert_eq!(command_line(&bytes, base + 0x100), None);
        assert_eq!(
            command_line(&answer(base, len + 2, 2 * word, &text), base),
            None
        );
        assert_eq!(command_line(&bytes[..word], base), None);
        assert_eq!(command_line(&bytes, 0), None);
    }
}
