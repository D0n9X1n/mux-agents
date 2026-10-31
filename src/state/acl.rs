//! Extended ACLs: an ACL entry beyond the mode bits can open a state folder to other users.

/// What a folder's ACL holds beyond its mode bits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Acl {
    /// Any extended entry: on macOS any ACE, on Linux a named user or group, a mask or a default ACL.
    pub extended: bool,
    /// macOS: an entry that is not a deny. A folder above the root may carry deny entries only.
    pub non_deny: bool,
}

/// Whether Linux `system.posix_acl_access` bytes (empty when the attribute is absent) and the
/// `system.posix_acl_default` bytes, when present, make an extended ACL. Bytes that do not parse count as
/// extended: an unreadable signal never passes.
pub fn posix_acl_extended(access: &[u8], default: Option<&[u8]>) -> bool {
    if default.is_some() {
        return true;
    }
    if access.is_empty() {
        return false;
    }
    // A u32 version 2 header, then 8-byte (u16 tag, u16 perm, u32 id) entries, little-endian.
    let Some((version, entries)) = access.split_first_chunk::<4>() else {
        return true;
    };
    if u32::from_le_bytes(*version) != 2 || entries.is_empty() || entries.len() % 8 != 0 {
        return true;
    }
    const USER: u16 = 0x02;
    const GROUP: u16 = 0x08;
    const MASK: u16 = 0x10;
    entries
        .chunks_exact(8)
        .any(|e| matches!(u16::from_le_bytes([e[0], e[1]]), USER | GROUP | MASK))
}

#[cfg(target_os = "linux")]
pub fn read(path: &std::path::Path) -> std::io::Result<Acl> {
    let access = xattr(path, c"system.posix_acl_access")?;
    let default = xattr(path, c"system.posix_acl_default")?;
    Ok(Acl {
        extended: posix_acl_extended(access.as_deref().unwrap_or_default(), default.as_deref()),
        non_deny: false,
    })
}

// lgetxattr of the path itself; `None` when absent or when the file system has no ACLs.
#[cfg(target_os = "linux")]
fn xattr(path: &std::path::Path, name: &std::ffi::CStr) -> std::io::Result<Option<Vec<u8>>> {
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut buf = vec![0u8; 1024];
    loop {
        // SAFETY: both names are NUL-terminated and the buffer pointer and length describe `buf`.
        let n = unsafe {
            libc::lgetxattr(
                path.as_ptr(),
                name.as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
            )
        };
        if n >= 0 {
            buf.truncate(n as usize);
            return Ok(Some(buf));
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ENODATA | libc::EOPNOTSUPP) => return Ok(None),
            Some(libc::ERANGE) if buf.len() < 1 << 16 => buf.resize(buf.len() * 4, 0),
            _ => return Err(error),
        }
    }
}

#[cfg(target_os = "macos")]
mod darwin {
    use std::ffi::{c_char, c_int, c_void};

    pub type AclT = *mut c_void;
    pub const ACL_TYPE_EXTENDED: c_int = 0x100;
    pub const ACL_FIRST_ENTRY: c_int = 0;
    pub const ACL_NEXT_ENTRY: c_int = -1;
    pub const ACL_EXTENDED_DENY: c_int = 2;

    // <sys/acl.h>; libc does not bind these.
    unsafe extern "C" {
        pub fn acl_get_link_np(path: *const c_char, kind: c_int) -> AclT;
        pub fn acl_get_entry(acl: AclT, entry_id: c_int, entry: *mut *mut c_void) -> c_int;
        pub fn acl_get_tag_type(entry: *mut c_void, tag: *mut c_int) -> c_int;
        pub fn acl_free(obj: *mut c_void) -> c_int;
    }
}

/// The ACEs of the path itself (a final link is not followed); inherited ones count too, as `ls -le` shows them.
#[cfg(target_os = "macos")]
pub fn read(path: &std::path::Path) -> std::io::Result<Acl> {
    use darwin::*;
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: `path` is NUL-terminated.
    let acl = unsafe { acl_get_link_np(path.as_ptr(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let error = io::Error::last_os_error();
        // ENOENT is "no ACL"; a vanished folder shows in the caller's second lstat.
        return match error.raw_os_error() {
            Some(libc::ENOENT) => Ok(Acl::default()),
            _ => Err(error),
        };
    }
    struct Free(AclT);
    impl Drop for Free {
        fn drop(&mut self) {
            // SAFETY: the ACL came from acl_get_link_np and is freed once.
            unsafe { acl_free(self.0) };
        }
    }
    let acl = Free(acl);
    let mut found = Acl::default();
    let mut entry_id = ACL_FIRST_ENTRY;
    loop {
        let mut entry = std::ptr::null_mut();
        // SAFETY: `acl.0` is a live ACL and `entry` a valid out pointer.
        if unsafe { acl_get_entry(acl.0, entry_id, &mut entry) } != 0 {
            // EINVAL ends the walk; anything else is a failed read.
            let error = io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::EINVAL) => Ok(found),
                _ => Err(error),
            };
        }
        entry_id = ACL_NEXT_ENTRY;
        let mut tag = 0;
        // SAFETY: `entry` was just returned for this live ACL.
        if unsafe { acl_get_tag_type(entry, &mut tag) } != 0 {
            return Err(io::Error::last_os_error());
        }
        found.extended = true;
        found.non_deny |= tag != ACL_EXTENDED_DENY;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER_OBJ: u16 = 0x01;
    const USER: u16 = 0x02;
    const GROUP_OBJ: u16 = 0x04;
    const GROUP: u16 = 0x08;
    const MASK: u16 = 0x10;
    const OTHER: u16 = 0x20;

    // The kernel's xattr form: a u32 version 2, then (u16 tag, u16 perm, u32 id) entries, little-endian.
    fn xattr(tags: &[u16]) -> Vec<u8> {
        let mut bytes = 2u32.to_le_bytes().to_vec();
        for &tag in tags {
            bytes.extend(tag.to_le_bytes());
            bytes.extend(7u16.to_le_bytes());
            bytes.extend(u32::MAX.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn linux_mask_acl_refuses() {
        let minimal = xattr(&[USER_OBJ, GROUP_OBJ, OTHER]);
        assert!(!posix_acl_extended(&[], None));
        assert!(!posix_acl_extended(&minimal, None));
        for extra in [USER, GROUP, MASK] {
            let tags = [USER_OBJ, extra, GROUP_OBJ, OTHER];
            assert!(posix_acl_extended(&xattr(&tags), None), "{extra:#x}");
        }
        // Wrong version, a torn entry or a bare header: unreadable, so extended.
        let mut wrong = minimal.clone();
        wrong[0] = 1;
        assert!(posix_acl_extended(&wrong, None));
        assert!(posix_acl_extended(&minimal[..minimal.len() - 1], None));
        assert!(posix_acl_extended(&minimal[..3], None));
    }

    #[test]
    fn linux_default_acl_refuses() {
        let minimal = xattr(&[USER_OBJ, GROUP_OBJ, OTHER]);
        assert!(posix_acl_extended(&minimal, Some(&minimal)));
        assert!(posix_acl_extended(&[], Some(&[])));
    }

    #[cfg(target_os = "macos")]
    use crate::state::private_unix::tests::chmod_a;

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_read_sees_allow_and_deny_aces() {
        let dir = crate::testing::TempDir::new();
        let path = dir.path();
        assert_eq!(read(path).unwrap(), Acl::default());
        chmod_a(path, "everyone deny chown");
        assert_eq!(
            read(path).unwrap(),
            Acl {
                extended: true,
                non_deny: false
            }
        );
        chmod_a(path, "everyone allow list");
        assert_eq!(
            read(path).unwrap(),
            Acl {
                extended: true,
                non_deny: true
            }
        );
    }
}
