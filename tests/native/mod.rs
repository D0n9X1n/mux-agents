//! The binary's own providers on a test's state root, and the fixtures integration tests share.
// Each test target uses a subset (PM call 16's root in Tasks 8-9, the 150 s wait in Task 9).
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

/// PM call 18: polls `ready` every 250 ms for at most 150 s on every OS; panics naming `what` and `diagnose()`.
pub fn wait_150<T>(
    what: &str,
    mut ready: impl FnMut() -> Option<T>,
    diagnose: impl Fn() -> String,
) -> T {
    let end = Instant::now() + Duration::from_secs(150);
    loop {
        if let Some(v) = ready() {
            return v;
        }
        assert!(
            Instant::now() < end,
            "no {what} within 150 s: {}",
            diagnose()
        );
        std::thread::sleep(Duration::from_millis(250));
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
        hc: Box::new(mux_agent::hc::NativeHc),
        clock: Box::new(mux_agent::ctx::SystemClock),
        admitted: false,
        tty: false,
    }
}

/// PM call 10 (and occupy-3, o3 call 6): a DACL that denies everyone PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION and PROCESS_VM_READ.
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
