//! The `serve` sandbox: memory cap, no new privileges, no sockets (seccomp)
//! and Landlock.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use common::{gz, tmpdir, Nifti};
use gnome_nifti_preview::sandbox;
use serde_json::Value;

/// Runs only as the child of the test below, which checks its output.
#[test]
#[ignore]
fn sandbox_child() {
    let Some(dir) = std::env::var_os("NP_SANDBOX_DIR").map(PathBuf::from) else { return };
    let report = sandbox::apply(1 << 30);
    println!("REPORT {report:?}");
    assert!(report.no_new_privs);
    assert_eq!(report.data_limit, Some(1 << 30));
    assert_eq!(std::fs::read(dir.join("readable")).unwrap(), b"x");
    let mut big: Vec<u8> = Vec::new();
    assert!(big.try_reserve_exact(2 << 30).is_err(), "2 GiB allocation under a 1 GiB limit");
    let mut small: Vec<u8> = Vec::new();
    assert!(small.try_reserve_exact(64 << 20).is_ok());
    if cfg!(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))) {
        assert!(report.seccomp, "seccomp filter not applied");
    }
    if report.seccomp {
        use std::os::unix::net::{UnixDatagram, UnixListener, UnixStream};
        assert!(std::net::UdpSocket::bind("127.0.0.1:0").is_err(), "UDP");
        assert!(std::net::TcpStream::connect("127.0.0.1:9").is_err(), "TCP");
        assert!(UnixStream::connect(dir.join("peer.sock")).is_err(), "Unix stream");
        assert!(UnixDatagram::unbound().is_err(), "Unix datagram");
        assert!(UnixStream::pair().is_err(), "socketpair");
        assert!(UnixListener::bind(dir.join("listen.sock")).is_err(), "Unix listener");
        // Reading still works, threads still start.
        assert_eq!(std::fs::read(dir.join("readable")).unwrap(), b"x");
        assert_eq!(std::thread::spawn(|| 7).join().unwrap(), 7);
    }
    if let Some(abi) = report.landlock {
        assert!(std::fs::File::create(dir.join("new")).is_err(), "create");
        assert!(std::fs::OpenOptions::new().write(true).open(dir.join("readable")).is_err(), "write");
        assert!(std::fs::remove_file(dir.join("readable")).is_err(), "remove");
        assert!(std::fs::create_dir(dir.join("sub")).is_err(), "mkdir");
        assert!(std::os::unix::fs::symlink("readable", dir.join("link")).is_err(), "symlink");
        assert!(Command::new("/bin/true").status().is_err(), "exec");
        if abi >= 4 {
            assert!(std::net::TcpListener::bind("127.0.0.1:0").is_err(), "bind");
        }
    }
    println!("CHILD OK");
}

#[test]
fn sandbox_blocks_writes_exec_network_and_big_allocations() {
    let dir = tmpdir("sandbox");
    std::fs::write(dir.join("readable"), b"x").unwrap();
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "sandbox_child", "--ignored", "--nocapture", "--test-threads=1"])
        .env("NP_SANDBOX_DIR", &dir)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success() && text.contains("CHILD OK"), "{text}\n{}", String::from_utf8_lossy(&out.stderr));
    if std::env::var_os("NP_REQUIRE_LANDLOCK").is_some() {
        assert!(text.contains("landlock: Some("), "Landlock not enforced on this kernel: {text}");
    }
    // Nothing was created or removed.
    let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, ["readable"]);
    std::fs::remove_dir_all(&dir).ok();
}

fn spawn_serve(env: &[(&str, &str)]) -> (std::process::Child, Value) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nifti-preview"));
    cmd.arg("serve").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.as_mut().unwrap()).read_line(&mut line).unwrap();
    (child, serde_json::from_str(&line).unwrap())
}

#[test]
fn serve_runs_sandboxed() {
    let (mut child, hello) = spawn_serve(&[("NIFTI_PREVIEW_MAX_BYTES", "1073741824"), ("NIFTI_PREVIEW_CACHE_BYTES", "0")]);
    let s = &hello["sandbox"];
    assert_eq!(s["no_new_privs"], true, "{hello}");
    if cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
        assert_eq!(s["seccomp"], true, "{hello}");
    }
    assert_eq!(s["data_limit"], sandbox::data_limit(1 << 30, 0), "{hello}");
    let status = std::fs::read_to_string(format!("/proc/{}/status", child.id())).unwrap();
    assert!(status.contains("NoNewPrivs:\t1"), "{status}");
    let limits = std::fs::read_to_string(format!("/proc/{}/limits", child.id())).unwrap();
    let data = limits.lines().find(|l| l.starts_with("Max data size")).unwrap().to_string();
    assert!(data.contains(&sandbox::data_limit(1 << 30, 0).to_string()), "{data}");
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
}

extern "C" {
    fn dup2(old: i32, new: i32) -> i32;
}

#[test]
fn serve_closes_inherited_descriptors() {
    use std::os::unix::io::AsRawFd;
    use std::os::unix::process::CommandExt;
    let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let fd = socket.as_raw_fd();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nifti-preview"));
    cmd.arg("serve").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    // SAFETY: dup2 is async-signal-safe; the child starts with the socket open
    // (without close-on-exec) as descriptor 9.
    unsafe {
        cmd.pre_exec(move || if dup2(fd, 9) < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) });
    }
    let mut child = cmd.spawn().unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.as_mut().unwrap()).read_line(&mut line).unwrap();
    let fds: Vec<String> = std::fs::read_dir(format!("/proc/{}/fd", child.id()))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(!fds.contains(&"9".to_string()), "inherited socket still open: {fds:?}");
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
}

/// Images just under the limit still load with the memory cap in place.
/// (Building 1 GB of test images takes minutes without optimisations: this
/// runs with `cargo test --release`, as in CI.)
#[test]
#[cfg_attr(debug_assertions, ignore)]
fn images_at_the_limit_load_under_the_memory_cap() {
    let dir = tmpdir("atlimit");
    let max: u64 = 512 << 20;
    let mut files = Vec::new();
    // gzip single volume (fast path), gzip 4D (streaming), .nii read and float64.
    let n = Nifti::new(&[1024, 1024, 500], 2);
    files.push(("single.nii.gz", gz(&n.file(&vec![1u8; 1024 * 1024 * 500]))));
    let n4 = Nifti::new(&[1024, 1024, 250, 2], 2);
    files.push(("4d.nii.gz", gz(&n4.file(&vec![2u8; 1024 * 1024 * 500]))));
    let f64n = Nifti::new(&[512, 512, 120], 64);
    files.push(("f64.nii", f64n.file(&vec![0u8; 512 * 512 * 120 * 8])));
    let (mut child, hello) = spawn_serve(&[
        ("NIFTI_PREVIEW_MAX_BYTES", &max.to_string()),
        ("NIFTI_PREVIEW_CACHE_BYTES", "67108864"),
    ]);
    assert!(hello["sandbox"]["data_limit"].as_u64().is_some());
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut ask = |req: Value| -> Value {
        writeln!(stdin, "{req}").unwrap();
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        if let Some(n) = v.get("bytes").and_then(Value::as_u64) {
            let mut skip = vec![0u8; n as usize];
            std::io::Read::read_exact(&mut out, &mut skip).unwrap();
        }
        v
    };
    for (name, bytes) in &files {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        let open = ask(serde_json::json!({"id": 1, "op": "open", "path": p}));
        assert_eq!(open["ok"], true, "{name}: {open}");
        let load = ask(serde_json::json!({"id": 2, "op": "load"}));
        assert_eq!(load["ok"], true, "{name}: {load}");
        for view in ["axial", "coronal", "sagittal"] {
            let s = ask(serde_json::json!({"id": 3, "op": "slice", "view": view, "index": 10}));
            assert_eq!(s["ok"], true, "{name} {view}: {s}");
        }
        std::fs::remove_file(&p).unwrap();
    }
    drop(stdin);
    assert!(child.wait().unwrap().success());
    std::fs::remove_dir_all(&dir).ok();
}
