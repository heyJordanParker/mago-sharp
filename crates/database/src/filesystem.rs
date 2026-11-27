//! What the filesystem that holds a path keeps of its files, as `statfs` names it.

use std::path::Path;

/// What the filesystem that holds a path keeps of its files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filesystem {
    /// A filesystem of this name whose client caches a file's status and contents, so its stat can be an old one while
    /// its bytes are new, and a change there reaches the operating system watcher late or not at all, as Watchman
    /// doesn't trust its change records. On Linux: FUSE's `FUSE_SUPER_MAGIC`, which a container's bind mount from
    /// macOS, virtiofs and sshfs are, 9p's `V9FS_MAGIC`, which WSL2 and Docker on Windows mount, `NFS_SUPER_MAGIC`, and
    /// `CIFS_SUPER_MAGIC`, `SMB2_SUPER_MAGIC` and `SMB_SUPER_MAGIC`. On macOS: `nfs`, `smbfs`, `webdav` and `afpfs`.
    Remote(&'static str),
    /// FAT's or exFAT's, which keep no status change time.
    WithoutChangeTime,
    /// Every other one, which records every write in a file's status change time and reports every change to the
    /// operating system watcher.
    Local,
}

/// The filesystem that holds `path`, or none where `statfs` can't name it, as on a platform other than Linux and macOS.
#[cfg(target_os = "linux")]
#[must_use]
pub fn of(path: &Path) -> Option<Filesystem> {
    statfs(path).map(|filesystem| of_magic(filesystem.f_type as u64))
}

/// The filesystem of the Linux magic number `magic`.
#[cfg(target_os = "linux")]
fn of_magic(magic: u64) -> Filesystem {
    match magic {
        0x4d44 | 0x2011_bab0 => Filesystem::WithoutChangeTime,
        0x6573_5546 => Filesystem::Remote("fuse"),
        0x0102_1997 => Filesystem::Remote("9p"),
        0x6969 => Filesystem::Remote("nfs"),
        0xff53_4d42 | 0xfe53_4d42 | 0x517b => Filesystem::Remote("cifs"),
        _ => Filesystem::Local,
    }
}

/// The filesystem that holds `path`, or none where `statfs` can't name it, as on a platform other than Linux and macOS.
#[cfg(target_os = "macos")]
#[must_use]
pub fn of(path: &Path) -> Option<Filesystem> {
    statfs(path).map(|filesystem| {
        let name: Vec<u8> =
            filesystem.f_fstypename.iter().take_while(|byte| **byte != 0).map(|byte| *byte as u8).collect();

        of_name(&name)
    })
}

/// The filesystem macOS names `name`.
#[cfg(target_os = "macos")]
fn of_name(name: &[u8]) -> Filesystem {
    match name {
        b"msdos" | b"exfat" => Filesystem::WithoutChangeTime,
        b"nfs" => Filesystem::Remote("nfs"),
        b"smbfs" => Filesystem::Remote("smbfs"),
        b"webdav" => Filesystem::Remote("webdav"),
        b"afpfs" => Filesystem::Remote("afpfs"),
        _ => Filesystem::Local,
    }
}

/// The filesystem that holds `path`, or none where `statfs` can't name it, as on a platform other than Linux and macOS.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[must_use]
pub fn of(_: &Path) -> Option<Filesystem> {
    None
}

/// The filesystem that holds `path`, as `statfs` gives it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn statfs(path: &Path) -> Option<libc::statfs> {
    use std::os::unix::ffi::OsStrExt;

    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `path` ends in a NUL byte, and `filesystem` has room for the `statfs` the call fills in.
    if unsafe { libc::statfs(path.as_ptr(), filesystem.as_mut_ptr()) } != 0 {
        return None;
    }

    // SAFETY: the call succeeded, so it filled `filesystem` in.
    Some(unsafe { filesystem.assume_init() })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    /// Linux's FAT and exFAT keep no status change time, and the stat of a FUSE, 9p, NFS, CIFS, SMB2 or SMB file can be
    /// one the client cached while the file's bytes are new. ext4 records every write, and holds the temp folder.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_names_fat_exfat_and_the_filesystems_whose_clients_cache_a_stat() {
        for magic in [0x4d44, 0x2011_bab0] {
            assert_eq!(of_magic(magic), Filesystem::WithoutChangeTime, "{magic:#x}");
        }
        for (magic, name) in [
            (0x6573_5546, "fuse"),
            (0x0102_1997, "9p"),
            (0x6969, "nfs"),
            (0xff53_4d42, "cifs"),
            (0xfe53_4d42, "cifs"),
            (0x517b, "cifs"),
        ] {
            assert_eq!(of_magic(magic), Filesystem::Remote(name), "{magic:#x}");
        }
        assert_eq!(of_magic(0xef53), Filesystem::Local, "ext4");
        let directory = tempfile::tempdir().expect("a temporary folder should be made");
        assert_eq!(of(directory.path()), Some(Filesystem::Local), "the temp folder's filesystem");
    }

    /// macOS's FAT and exFAT keep no status change time, and the stat of an NFS, SMB, WebDAV or AFP file can be one the
    /// client cached while the file's bytes are new. APFS records every write, and holds the temp folder.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_names_fat_exfat_and_the_filesystems_whose_clients_cache_a_stat() {
        for name in [b"msdos".as_slice(), b"exfat"] {
            assert_eq!(of_name(name), Filesystem::WithoutChangeTime, "{}", String::from_utf8_lossy(name));
        }
        for name in ["nfs", "smbfs", "webdav", "afpfs"] {
            assert_eq!(of_name(name.as_bytes()), Filesystem::Remote(name), "{name}");
        }
        assert_eq!(of_name(b"apfs"), Filesystem::Local, "APFS");
        let directory = tempfile::tempdir().expect("a temporary folder should be made");
        assert_eq!(of(directory.path()), Some(Filesystem::Local), "the temp folder's filesystem");
    }
}
