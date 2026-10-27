//! Defence in depth for `serve`. Once started, the backend only reads files
//! and talks over the stdin/stdout pipes it inherited, so it gives up the rest:
//!
//! * `RLIMIT_DATA`: heap and anonymous memory are capped a little above what
//!   the image limit allows, so an allocation that slipped past the checks
//!   fails inside this process instead of pushing the desktop into swap
//!   (read-only file mappings are not counted);
//! * `PR_SET_NO_NEW_PRIVS`;
//! * seccomp: no new sockets of any kind (TCP, UDP, Unix, netlink…), no
//!   io_uring, no system calls of another architecture. `serve` also closes
//!   every inherited descriptor but stdin, stdout and stderr first;
//! * Landlock (Linux 5.13+): no writing, creating, removing, truncating or
//!   executing files, no TCP bind/connect (Linux 6.7+), no abstract Unix
//!   sockets or signals to other processes (Linux 6.12+). Reading stays
//!   allowed everywhere.
//!
//! Every step is best effort: an older kernel simply provides less.

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Report {
    pub data_limit: Option<u64>,
    pub no_new_privs: bool,
    /// Socket creation denied by a seccomp filter.
    pub seccomp: bool,
    /// Landlock ABI version enforced, None when unavailable.
    pub landlock: Option<i64>,
}

/// Memory needed besides the voxel data: header and extensions, the
/// compressed copy of the gzip fast path, samples, scratch and slice
/// buffers, thread stacks.
pub const DATA_LIMIT_SLACK: u64 = (256 << 20) + (256 << 20) + (768 << 20);

pub fn data_limit(max_image_bytes: u64, cache_bytes: u64) -> u64 {
    max_image_bytes.saturating_add(cache_bytes).saturating_add(DATA_LIMIT_SLACK)
}

#[cfg(target_os = "linux")]
mod sys {
    use std::ffi::{c_int, c_long, c_void};

    extern "C" {
        pub fn prctl(option: c_int, ...) -> c_int;
        pub fn syscall(number: c_long, ...) -> c_long;
        pub fn getrlimit(resource: c_int, rlim: *mut Rlimit) -> c_int;
        pub fn setrlimit(resource: c_int, rlim: *const Rlimit) -> c_int;
        pub fn close(fd: c_int) -> c_int;
    }

    /// struct rlimit: two unsigned longs, 64-bit on the targets built.
    #[repr(C)]
    pub struct Rlimit {
        pub cur: u64,
        pub max: u64,
    }

    #[repr(C)]
    pub struct RulesetAttr {
        pub handled_access_fs: u64,
        pub handled_access_net: u64,
        pub scoped: u64,
    }

    #[repr(C)]
    pub struct SockFilter {
        pub code: u16,
        pub jt: u8,
        pub jf: u8,
        pub k: u32,
    }

    #[repr(C)]
    pub struct SockFprog {
        pub len: u16,
        pub filter: *const SockFilter,
    }

    pub const RLIMIT_DATA: c_int = 2;
    pub const PR_SET_NO_NEW_PRIVS: c_int = 38;
    pub const PR_SET_SECCOMP: c_int = 22;
    pub const SECCOMP_MODE_FILTER: c_int = 2;
    pub const SYS_CLOSE_RANGE: c_long = 436;
    // Same numbers on every architecture (unified syscall table).
    pub const SYS_LANDLOCK_CREATE_RULESET: c_long = 444;
    pub const SYS_LANDLOCK_RESTRICT_SELF: c_long = 446;
    pub const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;
    pub const NULL: *const c_void = std::ptr::null();
}

static APPLIED: std::sync::OnceLock<Report> = std::sync::OnceLock::new();

/// What `apply` enforced in this process (None if it was never called).
pub fn current() -> Option<&'static Report> {
    APPLIED.get()
}

/// Apply every restriction the kernel supports; never fails.
pub fn apply(data_limit: u64) -> Report {
    #[cfg(target_os = "linux")]
    let report = {
        let data_limit = limit_data(data_limit);
        let no_new_privs = no_new_privs();
        // seccomp needs no_new_privs (or privileges this process does not have).
        let seccomp = no_new_privs && deny_sockets();
        Report { data_limit, no_new_privs, seccomp, landlock: landlock() }
    };
    #[cfg(not(target_os = "linux"))]
    let report = {
        let _ = data_limit;
        Report::default()
    };
    let _ = APPLIED.set(report.clone());
    report
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
fn limit_data(bytes: u64) -> Option<u64> {
    use sys::*;
    let mut cur = Rlimit { cur: 0, max: 0 };
    // SAFETY: getrlimit writes one struct rlimit (two unsigned longs on Linux).
    if unsafe { getrlimit(RLIMIT_DATA, &mut cur) } != 0 {
        return None;
    }
    let new = Rlimit { cur: bytes.min(cur.cur), max: bytes.min(cur.max) };
    // SAFETY: setrlimit reads one struct rlimit; only lowering, never raising.
    if unsafe { setrlimit(RLIMIT_DATA, &new) } != 0 {
        return None;
    }
    Some(new.cur)
}

#[cfg(all(target_os = "linux", not(target_pointer_width = "64")))]
fn limit_data(_bytes: u64) -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn no_new_privs() -> bool {
    use std::ffi::c_ulong;
    // SAFETY: prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) takes integer arguments only.
    unsafe { sys::prctl(sys::PR_SET_NO_NEW_PRIVS, 1 as c_ulong, 0 as c_ulong, 0 as c_ulong, 0 as c_ulong) == 0 }
}

/// Close every descriptor above stderr: the backend only uses the pipes it was
/// given, and an inherited socket would bypass the seccomp filter below.
pub fn close_inherited_descriptors() {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::c_long;
        // SAFETY: close_range(3, ~0, 0) takes integers; nothing in this process
        // holds a descriptor above 2 yet (called first thing in `serve`).
        let done = unsafe { sys::syscall(sys::SYS_CLOSE_RANGE, 3 as c_long, u32::MAX as c_long, 0 as c_long) } == 0;
        if !done {
            // Kernels before 5.9: close them one by one.
            if let Ok(dir) = std::fs::read_dir("/proc/self/fd") {
                let fds: Vec<i32> = dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok()).filter(|&fd| fd > 2).collect();
                for fd in fds {
                    // SAFETY: closing a descriptor of this process; the directory
                    // handle is already dropped (its descriptor fails harmlessly).
                    unsafe {
                        sys::close(fd);
                    }
                }
            }
        }
    }
}

/// System calls that create sockets (or an io_uring, which can) fail with
/// EPERM, and so does any system call of another architecture's table.
#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
fn deny_sockets() -> bool {
    use std::ffi::{c_int, c_ulong};
    use sys::*;
    #[cfg(target_arch = "x86_64")]
    const ARCH: u32 = 0xC000_003E; // AUDIT_ARCH_X86_64
    #[cfg(target_arch = "aarch64")]
    const ARCH: u32 = 0xC000_00B7; // AUDIT_ARCH_AARCH64
    #[cfg(target_arch = "x86_64")]
    const DENIED: [u32; 3] = [41, 53, 425]; // socket, socketpair, io_uring_setup
    #[cfg(target_arch = "aarch64")]
    const DENIED: [u32; 3] = [198, 199, 425];
    const LD_W_ABS: u16 = 0x20;
    const JEQ_K: u16 = 0x15;
    const JGE_K: u16 = 0x35;
    const RET_K: u16 = 0x06;
    const ALLOW: u32 = 0x7fff_0000;
    const EPERM: u32 = 0x0005_0000 | 1;
    let op = |code, jt, jf, k| SockFilter { code, jt, jf, k };
    // seccomp_data: nr at offset 0, arch at offset 4.
    let mut prog = vec![op(LD_W_ABS, 0, 0, 4), op(JEQ_K, 1, 0, ARCH), op(RET_K, 0, 0, EPERM), op(LD_W_ABS, 0, 0, 0)];
    // Checks jump forward to the final EPERM, past the remaining checks and ALLOW.
    let mut checks: Vec<(u16, u32)> = DENIED.iter().map(|&nr| (JEQ_K, nr)).collect();
    if cfg!(target_arch = "x86_64") {
        checks.push((JGE_K, 0x4000_0000)); // the x32 ABI
    }
    let count = checks.len();
    for (i, (code, k)) in checks.into_iter().enumerate() {
        prog.push(op(code, (count - i) as u8, 0, k));
    }
    prog.push(op(RET_K, 0, 0, ALLOW));
    prog.push(op(RET_K, 0, 0, EPERM));
    let fprog = SockFprog { len: prog.len() as u16, filter: prog.as_ptr() };
    // SAFETY: fprog points to `prog`, which outlives the call; the kernel copies it.
    unsafe {
        prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER as c_ulong, &fprog as *const SockFprog as c_ulong, 0 as c_ulong, 0 as c_ulong)
            == 0 as c_int
    }
}

#[cfg(all(target_os = "linux", not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
fn deny_sockets() -> bool {
    false
}

#[cfg(target_os = "linux")]
fn landlock() -> Option<i64> {
    use std::ffi::c_long;
    use sys::*;
    // SAFETY: the version query takes a null attribute pointer and size 0.
    let abi = unsafe { syscall(SYS_LANDLOCK_CREATE_RULESET, NULL as c_long, 0 as c_long, LANDLOCK_CREATE_RULESET_VERSION as c_long) };
    if abi < 1 {
        return None;
    }
    let abi = abi as i64;
    // Everything but reading files and listing directories is handled, and
    // no rule grants it back: it is denied everywhere.
    let mut fs: u64 = (1 << 0) // EXECUTE
        | (1 << 1) // WRITE_FILE
        | (1 << 4) | (1 << 5) // REMOVE_DIR, REMOVE_FILE
        | (1 << 6) | (1 << 7) | (1 << 8) | (1 << 9) | (1 << 10) | (1 << 11) | (1 << 12); // MAKE_*
    if abi >= 2 {
        fs |= 1 << 13; // REFER
    }
    if abi >= 3 {
        fs |= 1 << 14; // TRUNCATE
    }
    if abi >= 5 {
        fs |= 1 << 15; // IOCTL_DEV
    }
    let net: u64 = if abi >= 4 { (1 << 0) | (1 << 1) } else { 0 }; // BIND_TCP, CONNECT_TCP
    let scoped: u64 = if abi >= 6 { (1 << 0) | (1 << 1) } else { 0 }; // ABSTRACT_UNIX_SOCKET, SIGNAL
    let attr = RulesetAttr { handled_access_fs: fs, handled_access_net: net, scoped };
    // The kernel reads only the fields this ABI knows.
    let size = match abi {
        1..=3 => 8usize,
        4 | 5 => 16,
        _ => 24,
    };
    // SAFETY: attr outlives the call and `size` does not exceed its size.
    let fd = unsafe { syscall(SYS_LANDLOCK_CREATE_RULESET, &attr as *const RulesetAttr as c_long, size as c_long, 0 as c_long) };
    if fd < 0 {
        return None;
    }
    // SAFETY: fd is the ruleset descriptor just created; flags must be 0.
    let ok = unsafe { syscall(SYS_LANDLOCK_RESTRICT_SELF, fd, 0 as c_long) } == 0;
    // SAFETY: closing our own descriptor once.
    unsafe {
        close(fd as std::ffi::c_int);
    }
    ok.then_some(abi)
}
