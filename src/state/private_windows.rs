//! Windows state folders private to the user, SYSTEM and Administrators (MuxAgents.ps1:423-485, D3).

use std::io;
use std::path::Path;

use crate::error::{Res, refuse};

pub const SYSTEM: &str = "S-1-5-18";
pub const ADMINS: &str = "S-1-5-32-544";

// The Win32 values, so the pure checks compile and test on every host.
#[cfg(any(windows, test))]
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// One DACL entry: allow or deny, its inheritance flags and its SID (`None` when the entry's form has none
/// we read, such as an object ACE).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ace {
    pub allow: bool,
    pub flags: u8,
    pub sid: Option<String>,
}

fn ours(user: &str, sid: &str) -> bool {
    sid == user || sid == SYSTEM || sid == ADMINS
}

/// Refuses 1 `state` unless the owner and every allow entry are `user`, SYSTEM or Administrators. `dacl` is
/// `None` for a null DACL, which grants everyone everything.
pub fn decide_acl(user: &str, owner: &str, dacl: Option<&[Ace]>) -> Res<()> {
    if !ours(user, owner) {
        return refuse(1, "state", "the state folder has another owner");
    }
    let shared = |ace: &Ace| ace.allow && !ace.sid.as_deref().is_some_and(|sid| ours(user, sid));
    match dacl {
        Some(aces) if !aces.iter().any(shared) => Ok(()),
        _ => refuse(1, "state", "the state folder is shared with other users"),
    }
}

/// Refuses 1 `state` when `full` or any existing ancestor is a junction or link. `attributes` gives a path's
/// own attributes (a final link not followed), `None` when nothing is there. A failed read is an error,
/// never a pass.
pub fn no_reparse(
    full: &Path,
    attributes: &mut dyn FnMut(&Path) -> io::Result<Option<u32>>,
) -> Res<()> {
    for path in full.ancestors().filter(|p| !p.as_os_str().is_empty()) {
        if attributes(path)?.is_some_and(|a| a & FILE_ATTRIBUTE_REPARSE_POINT != 0) {
            return refuse(1, "state", "the state folder path has a junction or link");
        }
    }
    Ok(())
}

#[cfg(windows)]
pub use win::{private_dirs, user_sid};

#[cfg(windows)]
mod win {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::MetadataExt;

    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce, GetTokenInformation,
        OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY,
        TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // <winnt.h> ACE types; Win32_System_SystemServices is not a crate feature.
    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0x0;
    const ACCESS_DENIED_ACE_TYPE: u8 = 0x1;
    const ACCESS_DENIED_OBJECT_ACE_TYPE: u8 = 0x6;
    const ACCESS_ALLOWED_CALLBACK_ACE_TYPE: u8 = 0x9;
    const ACCESS_DENIED_CALLBACK_ACE_TYPE: u8 = 0xA;
    const ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE: u8 = 0xC;

    struct Local(*mut core::ffi::c_void);
    impl Drop for Local {
        fn drop(&mut self) {
            // SAFETY: the pointer came from a Win32 call that allocates with LocalAlloc; freed once.
            unsafe { LocalFree(self.0) };
        }
    }

    pub(super) fn wide(path: &std::ffi::OsStr) -> io::Result<Vec<u16>> {
        let mut wide: Vec<u16> = path.encode_wide().collect();
        if wide.contains(&0) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        wide.push(0);
        Ok(wide)
    }

    fn sid_text(sid: PSID) -> io::Result<String> {
        let mut text = std::ptr::null_mut();
        // SAFETY: `sid` is a valid SID owned by the caller; `text` receives a LocalAlloc string.
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let _free = Local(text.cast());
        // SAFETY: `text` is a NUL-terminated wide string.
        let len = (0..).take_while(|&i| unsafe { *text.add(i) } != 0).count();
        // SAFETY: `len` units precede the NUL.
        Ok(String::from_utf16_lossy(unsafe {
            std::slice::from_raw_parts(text, len)
        }))
    }

    /// The current user's SID; the token handle is closed at once.
    pub fn user_sid() -> Res<String> {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: the pseudo handle needs no closing; `token` is a valid out pointer.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        struct Token(HANDLE);
        impl Drop for Token {
            fn drop(&mut self) {
                // SAFETY: an open token handle, closed once.
                unsafe { CloseHandle(self.0) };
            }
        }
        let token = Token(token);
        let mut buf = vec![0u64; 64];
        let mut len = 0u32;
        // SAFETY: the buffer is 8-byte aligned and its length is passed.
        let ok = unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buf.as_mut_ptr().cast(),
                (buf.len() * 8) as u32,
                &mut len,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: GetTokenInformation filled a TOKEN_USER at the start of `buf`.
        let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
        Ok(sid_text(user.User.Sid)?)
    }

    fn attributes(path: &Path) -> io::Result<Option<u32>> {
        match std::fs::symlink_metadata(path) {
            Ok(meta) => Ok(Some(meta.file_attributes())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Creates each path (and missing parents) private to the user, SYSTEM and Administrators; an existing
    /// folder is checked, never repaired.
    pub fn private_dirs(paths: &[&Path]) -> Res<()> {
        let sid = user_sid()?;
        for path in paths {
            private_dir(&sid, &std::path::absolute(path)?)?;
        }
        Ok(())
    }

    fn private_dir(sid: &str, path: &Path) -> Res<()> {
        no_reparse(path, &mut attributes)?;
        if let Some(attrs) = attributes(path)? {
            if attrs & FILE_ATTRIBUTE_DIRECTORY == 0 {
                return refuse(1, "state", "the state folder path is a file");
            }
            return check(sid, path);
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
            && attributes(parent)?.is_none()
        {
            private_dir(sid, parent)?;
        }
        create(sid, path)?;
        check(sid, path)
    }

    // A protected DACL (nothing inherited) of full control for the user, SYSTEM and Administrators,
    // inherited by everything below, and the user as owner.
    fn create(sid: &str, path: &Path) -> Res<()> {
        let sddl = format!("O:{sid}D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)");
        let sddl = wide(sddl.as_ref())?;
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated; `sd` receives a LocalAlloc descriptor.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error().into());
        }
        let _free = Local(sd);
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd,
            bInheritHandle: 0,
        };
        let name = wide(path.as_os_str())?;
        // SAFETY: `name` is NUL-terminated and `attributes` points at a live descriptor.
        if unsafe { CreateDirectoryW(name.as_ptr(), &attributes) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }

    fn check(sid: &str, path: &Path) -> Res<()> {
        no_reparse(path, &mut attributes)?;
        let (owner, dacl) = read(path)?;
        decide_acl(sid, &owner, dacl.as_deref())
    }

    /// The owner SID and the DACL entries (`None` for a null DACL), inherited ones included.
    pub(super) fn read(path: &Path) -> io::Result<(String, Option<Vec<Ace>>)> {
        let name = wide(path.as_os_str())?;
        let (mut owner, mut dacl): (PSID, *mut ACL) = (std::ptr::null_mut(), std::ptr::null_mut());
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `name` is NUL-terminated; the out pointers point into `sd`, freed below.
        let status = unsafe {
            GetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut sd,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let _free = Local(sd);
        if owner.is_null() {
            return Err(io::Error::other("no owner"));
        }
        let owner = sid_text(owner)?;
        if dacl.is_null() {
            return Ok((owner, None));
        }
        // SAFETY: `dacl` points at a valid ACL inside `sd`.
        let count = unsafe { (*dacl).AceCount };
        let mut aces = Vec::with_capacity(count as usize);
        for index in 0..u32::from(count) {
            let mut ace = std::ptr::null_mut();
            // SAFETY: `index` is below the ACL's count.
            if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: every ACE starts with an ACE_HEADER.
            let header = unsafe { *(ace as *const ACE_HEADER) };
            let (allow, sid) = match header.AceType {
                // ACCESS_ALLOWED_ACE and its callback form: header, mask, then the SID.
                ACCESS_ALLOWED_ACE_TYPE | ACCESS_ALLOWED_CALLBACK_ACE_TYPE => {
                    // SAFETY: the SID starts 8 bytes in (4-byte header, 4-byte mask).
                    let sid = unsafe { (ace as *mut u8).add(8) };
                    (true, Some(sid_text(sid.cast())?))
                }
                ACCESS_DENIED_ACE_TYPE
                | ACCESS_DENIED_OBJECT_ACE_TYPE
                | ACCESS_DENIED_CALLBACK_ACE_TYPE
                | ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE => (false, None),
                // Object or unknown allow forms: a SID we do not read counts as someone else.
                _ => (true, None),
            };
            aces.push(Ace {
                allow,
                flags: header.AceFlags,
                sid,
            });
        }
        Ok((owner, Some(aces)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Refusal;

    const USER: &str = "S-1-5-21-1-2-3-1001";

    fn allow(sid: &str) -> Ace {
        Ace {
            allow: true,
            flags: 3,
            sid: Some(sid.into()),
        }
    }

    fn message(result: Res<()>) -> String {
        match result {
            Ok(()) => "ok".into(),
            Err(Refusal {
                code,
                check,
                message,
            }) => format!("{code} {check} {message}"),
        }
    }

    #[test]
    fn acl_decision_allows_user_system_admins_only() {
        let shared = "1 state the state folder is shared with other users";
        let ours = [allow(USER), allow(SYSTEM), allow(ADMINS)];
        assert_eq!(message(decide_acl(USER, USER, Some(&ours))), "ok");
        assert_eq!(message(decide_acl(USER, USER, Some(&[]))), "ok");
        // A deny for anyone passes, whatever it names or lacks.
        let deny = Ace {
            allow: false,
            flags: 0,
            sid: None,
        };
        let everyone_denied = [allow(USER), deny];
        assert_eq!(
            message(decide_acl(USER, USER, Some(&everyone_denied))),
            "ok"
        );
        for other in [
            allow("S-1-1-0"),
            allow("S-1-5-11"),
            allow("S-1-5-21-1-2-3-1002"),
            allow("S-1-3-0"),
            Ace {
                allow: true,
                flags: 0,
                sid: None,
            },
        ] {
            let dacl = [allow(USER), other.clone()];
            assert_eq!(
                message(decide_acl(USER, USER, Some(&dacl))),
                shared,
                "{other:?}"
            );
        }
        // A null DACL grants everyone everything.
        assert_eq!(message(decide_acl(USER, USER, None)), shared);
    }

    #[test]
    fn acl_decision_refuses_other_owner() {
        let ours = [allow(USER)];
        for owner in [USER, SYSTEM, ADMINS] {
            assert_eq!(
                message(decide_acl(USER, owner, Some(&ours))),
                "ok",
                "{owner}"
            );
        }
        // The owner is checked first, before the entries.
        for owner in ["S-1-5-21-1-2-3-1002", "S-1-1-0"] {
            assert_eq!(
                message(decide_acl(USER, owner, None)),
                "1 state the state folder has another owner"
            );
        }
    }

    #[test]
    fn reparse_ancestor_refuses() {
        let dir = FILE_ATTRIBUTE_DIRECTORY;
        let mut seen = Vec::new();
        let attributes = |link: &'static str| {
            move |p: &Path| -> io::Result<Option<u32>> {
                Ok(match p.to_str().unwrap() {
                    "/c/a/b/k" => None,
                    p if p == link => Some(dir | FILE_ATTRIBUTE_REPARSE_POINT),
                    _ => Some(dir),
                })
            }
        };
        let full = Path::new("/c/a/b/k");
        let probe = attributes("/c/a");
        let refused = no_reparse(full, &mut |p| {
            seen.push(p.to_str().unwrap().to_owned());
            probe(p)
        });
        assert_eq!(
            message(refused),
            "1 state the state folder path has a junction or link"
        );
        // Deepest first; a missing path is skipped, not refused.
        assert_eq!(seen, ["/c/a/b/k", "/c/a/b", "/c/a"]);
        assert_eq!(message(no_reparse(full, &mut attributes("/none"))), "ok");
        // The path itself, and an unreadable ancestor.
        assert_eq!(
            message(no_reparse(Path::new("/c/a"), &mut attributes("/c/a"))),
            "1 state the state folder path has a junction or link"
        );
        let mut failing = |_: &Path| Err(io::Error::from(io::ErrorKind::PermissionDenied));
        assert_eq!(
            message(no_reparse(full, &mut failing)),
            "1 internal unexpected error (PermissionDenied)"
        );
    }

    // CI only: a real folder through CreateDirectoryW and GetNamedSecurityInfoW.
    #[cfg(windows)]
    #[test]
    fn creates_protected_dacl() {
        use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
        use windows_sys::Win32::Security::{
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl, SE_DACL_PROTECTED,
        };
        let dir = crate::testing::TempDir::new();
        let (a, b) = (dir.path().join("a"), dir.path().join("a").join("b"));
        assert_eq!(message(private_dirs(&[&b])), "ok");
        assert_eq!(message(private_dirs(&[&a, &b])), "ok");
        let user = user_sid().unwrap();
        for path in [&a, &b] {
            let (owner, dacl) = win::read(path).unwrap();
            assert_eq!(owner, user);
            let dacl = dacl.unwrap();
            let sids: Vec<_> = dacl.iter().map(|a| a.sid.clone().unwrap()).collect();
            assert_eq!(sids, [user.as_str(), SYSTEM, ADMINS], "{path:?}");
            // OBJECT_INHERIT | CONTAINER_INHERIT, explicit, not inherited from the temp folder.
            assert!(dacl.iter().all(|a| a.allow && a.flags == 3), "{dacl:?}");
            let name = win::wide(path.as_os_str()).unwrap();
            let mut sd = std::ptr::null_mut();
            let (mut control, mut revision) = (0u16, 0u32);
            // SAFETY: test-only read of the descriptor's control bits; the descriptor is leaked.
            unsafe {
                let status = GetNamedSecurityInfoW(
                    name.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut sd,
                );
                assert_eq!(status, 0);
                assert_ne!(
                    GetSecurityDescriptorControl(sd, &mut control, &mut revision),
                    0
                );
            }
            assert_ne!(control & SE_DACL_PROTECTED, 0);
        }
        let file = dir.path().join("f");
        std::fs::write(&file, b"").unwrap();
        assert_eq!(
            message(private_dirs(&[&file])),
            "1 state the state folder path is a file"
        );
    }
}
