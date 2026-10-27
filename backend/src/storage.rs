//! Where a file lives: a local disk, or a network file system (NFS, SMB/CIFS,
//! sshfs and the other FUSE mounts of remote storage, GVfs's FUSE bridge that
//! Files uses for sftp://, smb://…). On a network file system the backend
//! never maps the file (a dropped connection would kill it with SIGBUS),
//! reads it in large sequential blocks, and the preview asks before reading
//! much of it.
//!
//! `NIFTI_PREVIEW_ASSUME_NETWORK=1` makes every file count as remote (tests).

use std::fs::File;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Storage {
    pub network: bool,
    /// The file system type, as `/proc/self/mountinfo` names it when known
    /// (`nfs4`, `cifs`, `fuse.sshfs`…), else from `statfs`.
    pub fs: String,
}

/// File systems reached over the network, by `statfs` magic number.
const NETWORK_MAGIC: &[(i64, &str)] = &[
    (0x6969, "nfs"),
    (0x517B, "smb"),
    (0xFF53_4D42u32 as i64, "cifs"),
    (0xFE53_4D42u32 as i64, "smb2"),
    (0x0102_1997, "9p"),
    (0x00C3_6400, "ceph"),
    (0x5346_414F, "afs"),
    (0x7375_7245, "coda"),
    (0x564C, "ncp"),
    (0x0BD0_0BD0, "lustre"),
    (0x4750_4653, "gpfs"),
    (0x0116_1970, "gfs2"),
    (0x7461_636F, "ocfs2"),
    (0x6B41_4653, "kafs"),
];
const FUSE_MAGIC: i64 = 0x6573_5546;

/// FUSE file systems that reach remote storage (by their `fuse.` subtype).
const NETWORK_FUSE: &[&str] = &[
    "sshfs", "gvfsd-fuse", "rclone", "s3fs", "goofys", "gcsfuse", "davfs", "curlftpfs", "smbnetfs", "glusterfs",
    "ceph-fuse", "juicefs", "mountpoint-s3", "blobfuse", "blobfuse2", "onedriver", "google-drive-ocamlfuse",
    "httpdirfs", "nfs", "seaweedfs", "lizardfs", "moosefs", "mfs", "beegfs", "cvmfs", "xrootdfs",
];

/// Kernel types that are remote (as mountinfo names them).
fn network_type(fstype: &str) -> bool {
    let t = fstype.strip_prefix("fuse.").map(|s| (true, s)).unwrap_or((false, fstype));
    match t {
        (true, sub) => NETWORK_FUSE.contains(&sub),
        (false, k) => matches!(
            k,
            "nfs" | "nfs4" | "cifs" | "smb3" | "smbfs" | "9p" | "ceph" | "afs" | "coda" | "ncpfs" | "lustre" | "gpfs"
                | "gfs2" | "ocfs2" | "glusterfs" | "davfs" | "sshfs" | "orangefs" | "beegfs"
        ),
    }
}

/// Decode the octal escapes of mountinfo (`\040` for a space).
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The type of the mount holding `path` (the longest mount point that is a
/// prefix of it), from the text of `/proc/self/mountinfo`.
pub fn mount_type(mountinfo: &str, path: &Path) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for line in mountinfo.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        let Some(sep) = fields.iter().position(|f| *f == "-") else { continue };
        if fields.len() < 5 || sep + 1 >= fields.len() {
            continue;
        }
        let point = unescape(fields[4]);
        let p = Path::new(&point);
        if path.starts_with(p) {
            let len = point.len();
            if best.as_ref().is_none_or(|(l, _)| len >= *l) {
                best = Some((len, fields[sep + 1].to_string()));
            }
        }
    }
    best.map(|(_, t)| t)
}

#[cfg(target_os = "linux")]
fn statfs_magic(file: &File) -> Option<i64> {
    use std::os::fd::AsRawFd;
    // struct statfs on 64-bit Linux: f_type first (a long), then 15 more words.
    #[repr(C)]
    struct StatFs {
        f_type: i64,
        rest: [u64; 15],
    }
    extern "C" {
        fn fstatfs(fd: std::ffi::c_int, buf: *mut StatFs) -> std::ffi::c_int;
    }
    let mut buf = StatFs { f_type: 0, rest: [0; 15] };
    // SAFETY: the buffer is at least as large as struct statfs on the 64-bit
    // targets built, and the descriptor is ours.
    let rc = unsafe { fstatfs(file.as_raw_fd(), &mut buf) };
    (rc == 0).then_some(buf.f_type)
}

#[cfg(not(target_os = "linux"))]
fn statfs_magic(_file: &File) -> Option<i64> {
    None
}

/// Where the open `file` (at `path`) lives.
pub fn of(file: &File, path: &Path) -> Storage {
    if std::env::var_os("NIFTI_PREVIEW_ASSUME_NETWORK").is_some_and(|v| v == "1") {
        return Storage { network: true, fs: "test".into() };
    }
    let magic = statfs_magic(file);
    let mounted = || {
        let text = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
        let full = std::path::absolute(path).ok()?;
        let real = std::fs::canonicalize(&full).unwrap_or(full);
        mount_type(&text, &real)
    };
    match magic {
        Some(m) if m == FUSE_MAGIC => {
            let t = mounted().unwrap_or_else(|| "fuse".into());
            Storage { network: network_type(&t), fs: t }
        }
        Some(m) => match NETWORK_MAGIC.iter().find(|(k, _)| *k == m) {
            Some((_, name)) => Storage { network: true, fs: mounted().unwrap_or_else(|| name.to_string()) },
            None => Storage { network: false, fs: String::new() },
        },
        None => match mounted() {
            Some(t) => Storage { network: network_type(&t), fs: t },
            None => Storage::default(),
        },
    }
}

/// Read the file sequentially from now on (best effort).
#[cfg(target_os = "linux")]
pub fn advise_sequential(file: &File) {
    use std::os::fd::AsRawFd;
    extern "C" {
        fn posix_fadvise(fd: std::ffi::c_int, offset: i64, len: i64, advice: std::ffi::c_int) -> std::ffi::c_int;
    }
    const POSIX_FADV_SEQUENTIAL: std::ffi::c_int = 2;
    // SAFETY: an advisory call on our own descriptor.
    unsafe {
        posix_fadvise(file.as_raw_fd(), 0, 0, POSIX_FADV_SEQUENTIAL);
    }
}

#[cfg(not(target_os = "linux"))]
pub fn advise_sequential(_file: &File) {}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTINFO: &str = "\
22 1 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw
40 22 0:36 / /home rw,relatime shared:20 - btrfs /dev/nvme0n1p3 rw
91 40 0:52 / /home/user/remote rw,nosuid,nodev,relatime shared:50 - fuse.sshfs user@server:/data rw,user_id=1000
92 30 0:53 / /run/user/1000/gvfs rw,nosuid,nodev,relatime shared:51 - fuse.gvfsd-fuse gvfsd-fuse rw,user_id=1000
93 22 0:54 / /mnt/lab\\040share rw,relatime shared:52 - cifs //nas/lab rw,vers=3.1.1
94 22 0:55 / /mnt/nfs rw,relatime shared:53 - nfs4 nas:/export rw
95 30 0:56 / /run/user/1000/doc rw,nosuid,nodev,relatime shared:54 - fuse.portal portal rw
";

    #[test]
    fn mount_of_a_path() {
        let t = |p: &str| mount_type(MOUNTINFO, Path::new(p));
        assert_eq!(t("/home/user/remote/a.nii").as_deref(), Some("fuse.sshfs"));
        assert_eq!(t("/home/user/remote2/a.nii").as_deref(), Some("btrfs"));
        assert_eq!(t("/run/user/1000/gvfs/sftp:host=x/a.nii").as_deref(), Some("fuse.gvfsd-fuse"));
        assert_eq!(t("/mnt/lab share/x.dcm").as_deref(), Some("cifs"));
        assert_eq!(t("/etc/passwd").as_deref(), Some("ext4"));
    }

    #[test]
    fn remote_types() {
        for t in ["nfs4", "cifs", "fuse.sshfs", "fuse.gvfsd-fuse", "fuse.rclone", "9p", "smb3"] {
            assert!(network_type(t), "{t}");
        }
        for t in ["ext4", "btrfs", "xfs", "tmpfs", "fuse.portal", "fuseblk", "fuse.ntfs-3g", "overlay", "virtiofs"] {
            assert!(!network_type(t), "{t}");
        }
    }

    #[test]
    fn this_file_is_local() {
        let f = File::open(file!()).unwrap();
        if std::env::var_os("NIFTI_PREVIEW_ASSUME_NETWORK").is_none() {
            let s = of(&f, Path::new(file!()));
            // CI and developer machines build on local disks.
            assert!(!s.network || s.fs.starts_with("nfs") || s.fs.starts_with("fuse."), "{s:?}");
        }
    }
}
