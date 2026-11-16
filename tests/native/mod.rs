//! The binary's own providers on a test's state root, and the fixtures integration tests share.
// Each test target uses a subset (PM call 16's root in Tasks 8-9, the 150 s readiness deadline in Task 9).
#![allow(dead_code)]
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// PM call 16: a state root the production privacy check accepts: its parent exists; the root is mode 0700 on Unix.
pub fn private_root(state: &Path) {
    if !state.exists() {
        std::fs::create_dir(state).unwrap();
    }
    #[cfg(unix)]
    std::fs::set_permissions(state, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
}

/// PM call 18: a new member is ready within 150 s on every OS.
pub const READY: Duration = Duration::from_secs(150);

/// r2b call 35: one readiness deadline, started at the launch boundary (just before `spawn`). Every wait up to
/// `ready.json` spends what is left of it; an answer found after it is a failure.
pub struct Deadline {
    end: Instant,
    budget: Duration,
}

impl Deadline {
    pub fn start(budget: Duration) -> Deadline {
        Deadline {
            end: Instant::now() + budget,
            budget,
        }
    }

    /// Polls `ready` every 250 ms (at most until the end); panics naming `what` and `diagnose()` once the deadline has
    /// passed, even when the poll that took it past the end answered.
    pub fn wait<T>(
        &self,
        what: &str,
        mut ready: impl FnMut() -> Option<T>,
        diagnose: impl Fn() -> String,
    ) -> T {
        loop {
            let found = ready();
            let now = Instant::now();
            assert!(
                now < self.end,
                "no {what} within {:?} of the launch: {}",
                self.budget,
                diagnose()
            );
            if let Some(v) = found {
                return v;
            }
            std::thread::sleep((self.end - now).min(Duration::from_millis(250)));
        }
    }
}

pub fn ctx(state: &Path) -> mux_agent::ctx::Ctx {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap();
    mux_agent::ctx::Ctx {
        env: mux_agent::ctx::Env::from_os(std::env::vars_os()),
        pid: std::process::id(),
        cwd: std::env::current_dir().unwrap(),
        home,
        state_root: state.to_path_buf(),
        exe: PathBuf::from(env!("CARGO_BIN_EXE_mux-agent")),
        os: mux_agent::proc::host(),
        mux: std::cell::RefCell::new(None),
        member_mux: std::cell::RefCell::new(mux_agent::mux::MemberMux::Caller),
        hc: Box::new(mux_agent::hc::NativeHc),
        clock: Box::new(mux_agent::ctx::SystemClock),
        admitted: false,
        tty: false,
    }
}

/// PM call 10 (and occupy-3, o3 call 6): a DACL that denies everyone PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION and PROCESS_VM_READ.
/// r2b call 24: an enabled SeDebugPrivilege lets `OpenProcess` past any DACL, and GitHub's `windows-latest` runs the
/// tests elevated with it enabled (observed: High integrity, Administrators enabled, SeDebugPrivilege Enabled). So this
/// process drops it first: the deny then holds for this process's reads, and for every reader it starts afterwards
/// (a child copies the token's privilege states), as it does for a standard user.
#[cfg(windows)]
pub fn deny_queries(child: &std::process::Child) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SetKernelObjectSecurity,
    };
    disable_debug_privilege();
    let sddl: Vec<u16> = "D:(D;;0x1410;;;WD)".encode_utf16().chain([0]).collect();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: a NUL-terminated SDDL string and a valid out pointer; `sd` is freed below.
    let made = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(made, 0, "SDDL");
    // SAFETY: the child's own process handle, open with WRITE_DAC; `sd` came from the call above.
    let set =
        unsafe { SetKernelObjectSecurity(child.as_raw_handle(), DACL_SECURITY_INFORMATION, sd) };
    // SAFETY: allocated by ConvertString...W.
    unsafe { LocalFree(sd) };
    assert_ne!(set, 0, "SetKernelObjectSecurity");
}

/// Disables SeDebugPrivilege in this process's token (r2b call 24). A token without it (a standard user) is left as it
/// is (`ERROR_NOT_ALL_ASSIGNED`); any other failure panics, so the deny never silently depends on the runner.
#[cfg(windows)]
fn disable_debug_privilege() {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_NOT_ALL_ASSIGNED, ERROR_SUCCESS, GetLastError, HANDLE, LUID,
    };
    use windows_sys::Win32::Security::{
        AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, TOKEN_ADJUST_PRIVILEGES,
        TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let name: Vec<u16> = "SeDebugPrivilege".encode_utf16().chain([0]).collect();
    let mut luid = LUID {
        LowPart: 0,
        HighPart: 0,
    };
    // SAFETY: a NUL-terminated name and a valid out pointer.
    assert_ne!(
        unsafe { LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid) },
        0,
        "LookupPrivilegeValueW(SeDebugPrivilege)"
    );
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: this process's pseudo handle and a valid out pointer; the token is closed below.
    assert_ne!(
        unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                &mut token,
            )
        },
        0,
        "OpenProcessToken"
    );
    // Attributes 0: disabled. OpenProcess honours only enabled privileges.
    let off = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: 0,
        }],
    };
    // SAFETY: an open token with TOKEN_ADJUST_PRIVILEGES and one privilege; the previous state is not asked for.
    let adjusted = unsafe {
        AdjustTokenPrivileges(
            token,
            0,
            &off,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    // SAFETY: no preconditions; read right after the call it describes.
    let error = unsafe { GetLastError() };
    // SAFETY: the token opened above, closed once.
    unsafe { CloseHandle(token) };
    assert_ne!(adjusted, 0, "AdjustTokenPrivileges");
    assert!(
        error == ERROR_SUCCESS || error == ERROR_NOT_ALL_ASSIGNED,
        "AdjustTokenPrivileges: error {error}"
    );
}

/// What a deny depends on, for a precondition that fails: this process's privileges and integrity (`whoami`), the DACL
/// the child carries now, and what a fresh `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` of it answers.
#[cfg(windows)]
pub fn deny_report(child: &std::process::Child) -> String {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
        SE_KERNEL_OBJECT,
    };
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    let whoami = |arg: &str| match std::process::Command::new("whoami").arg(arg).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(e) => format!("whoami {arg}: {e}"),
    };
    let squeeze = |l: &str| l.split_whitespace().collect::<Vec<_>>().join(" ");
    let privileges: Vec<String> = whoami("/priv")
        .lines()
        .filter(|l| l.starts_with("Se"))
        .map(squeeze)
        .collect();
    let groups: Vec<String> = whoami("/groups")
        .lines()
        .filter(|l| l.contains("Mandatory Label") || l.contains("S-1-5-32-544"))
        .map(squeeze)
        .collect();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: the child's own handle (CreateProcess grants READ_CONTROL); the out pointers are valid or null; `sd` is
    // freed below.
    let got = unsafe {
        GetSecurityInfo(
            child.as_raw_handle(),
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    let dacl = if got != 0 {
        format!("GetSecurityInfo error {got}")
    } else {
        let mut text: *mut u16 = std::ptr::null_mut();
        let mut len = 0u32;
        // SAFETY: `sd` came from GetSecurityInfo; valid out pointers; `text` is freed below.
        let made = unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                sd,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                &mut len,
            )
        };
        let shown = if made == 0 {
            // SAFETY: no preconditions.
            format!("ConvertSecurityDescriptorToString error {}", unsafe {
                GetLastError()
            })
        } else {
            // SAFETY: `len` UTF-16 units written at `text` by the call above.
            let units = unsafe { std::slice::from_raw_parts(text, len as usize) };
            let s = String::from_utf16_lossy(units)
                .trim_end_matches('\0')
                .to_string();
            // SAFETY: allocated by ConvertSecurityDescriptorToStringSecurityDescriptorW.
            unsafe { LocalFree(text.cast()) };
            s
        };
        // SAFETY: allocated by GetSecurityInfo.
        unsafe { LocalFree(sd) };
        shown
    };
    // SAFETY: a plain open; null on failure.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, child.id()) };
    let open = if handle.is_null() {
        // SAFETY: no preconditions.
        format!("refused, error {}", unsafe { GetLastError() })
    } else {
        // SAFETY: the handle opened above, closed once.
        unsafe { CloseHandle(handle) };
        "opened".to_string()
    };
    format!(
        "privileges {privileges:?}; groups {groups:?}; child DACL {dacl}; OpenProcess(QUERY_LIMITED) {open}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // r2b call 35: an answer found after the deadline is a failure, not a success.
    #[test]
    #[should_panic(expected = "within")]
    fn a_deadline_rejects_an_answer_found_after_it() {
        let d = Deadline::start(Duration::from_millis(50));
        d.wait(
            "a late answer",
            || {
                std::thread::sleep(Duration::from_millis(100));
                Some(())
            },
            String::new,
        );
    }

    // r2b call 35: time spent before a wait (the spawn, an earlier wait) is charged to the one deadline.
    #[test]
    #[should_panic(expected = "within")]
    fn a_deadline_charges_time_spent_before_the_wait() {
        let d = Deadline::start(Duration::from_millis(50));
        std::thread::sleep(Duration::from_millis(100));
        d.wait(
            "an answer after the budget is spent",
            || Some(()),
            String::new,
        );
    }

    #[test]
    fn a_deadline_returns_an_answer_found_within_it() {
        let d = Deadline::start(Duration::from_secs(60));
        let mut polls = 0;
        let v = d.wait(
            "the third poll",
            || {
                polls += 1;
                (polls == 3).then_some(7)
            },
            String::new,
        );
        assert_eq!((v, polls), (7, 3));
    }
}
