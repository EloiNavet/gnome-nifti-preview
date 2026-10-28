//! The viewer inside a real terminal emulator, tmux: it starts, draws in
//! colour, follows keys and quits, and never sends tmux an image protocol
//! it would not pass on. Skipped when tmux is missing, unless
//! NITTY_REQUIRE_TMUX is set (CI).

mod common;

use std::process::Command;
use std::time::{Duration, Instant};

use common::{head, tmpdir, within};

/// A tmux server of its own. Its socket is named (`-L`), so that it lives
/// in tmux's short folder: a socket path in a deep TMPDIR can pass the 108
/// bytes a Unix socket address holds.
struct Tmux {
    socket: String,
}

impl Tmux {
    fn new() -> Tmux {
        Tmux { socket: format!("nitty-test-{}", std::process::id()) }
    }

    fn cmd(&self, args: &[&str]) -> std::process::Output {
        Command::new("tmux").args(["-L", &self.socket]).args(args).env_remove("TMUX").output().expect("tmux runs")
    }

    fn capture(&self, colours: bool) -> String {
        let mut a = vec!["capture-pane", "-p", "-t", "t"];
        if colours {
            a.push("-e");
        }
        String::from_utf8_lossy(&self.cmd(&a).stdout).into_owned()
    }

    fn until(&self, secs: u64, what: &str) -> String {
        let end = Instant::now() + Duration::from_secs(secs);
        loop {
            let s = self.capture(false);
            if s.contains(what) {
                return s;
            }
            assert!(Instant::now() < end, "{what:?} never shown in tmux:\n{s}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn alive(&self) -> bool {
        self.cmd(&["has-session", "-t", "t"]).status.success()
    }
}

impl Drop for Tmux {
    fn drop(&mut self) {
        // tmux leaves its socket behind: removed too.
        let path = String::from_utf8_lossy(&self.cmd(&["display-message", "-p", "#{socket_path}"]).stdout).trim().to_string();
        let _ = self.cmd(&["kill-server"]);
        if path.ends_with(&self.socket) {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn tmux_available() -> bool {
    let ok = Command::new("tmux").arg("-V").output().is_ok_and(|o| o.status.success());
    if !ok && std::env::var_os("NITTY_REQUIRE_TMUX").is_some() {
        panic!("NITTY_REQUIRE_TMUX is set but tmux cannot run");
    }
    ok
}

#[test]
fn inside_tmux() {
    if !tmux_available() {
        eprintln!("tmux not found: skipped");
        return;
    }
    within(60, "tmux session", || {
        let dir = tmpdir("tmux");
        let f = head(&dir, "head.nii.gz");
        let t = Tmux::new();
        let command = format!("{} --colormap gray '{}'; echo \"nitty exited with $?\"; sleep 30", common::BIN, f.display());
        let o = t.cmd(&["-f", "/dev/null", "new-session", "-d", "-x", "130", "-y", "40", "-s", "t", "-e", "COLORTERM=truecolor", &command]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let screen = t.until(15, "? help");
        assert!(screen.contains("Axial 18/35") && screen.contains("voxel 20 24 18"), "{screen}");
        // tmux answers for itself: text cells, or Sixel when tmux draws it.
        assert!(screen.contains("blocks") || screen.contains("sixel"), "{screen}");
        assert!(!screen.contains("kitty") && !screen.contains("iterm2"));
        // Colours reach tmux's cells.
        let coloured = t.capture(true);
        assert!(coloured.contains("\x1b[") && (coloured.contains("38;2;") || coloured.contains("48;2;") || coloured.contains("48;5;")), "no colour");
        t.cmd(&["send-keys", "-t", "t", "Right"]);
        t.until(10, "voxel 21 24 18");
        t.cmd(&["send-keys", "-t", "t", "PageUp"]);
        t.until(10, "Axial 19/35");
        t.cmd(&["send-keys", "-t", "t", "q"]);
        // Back to the shell's screen (the viewer left the alternate screen),
        // with a status of 0.
        let screen = t.until(10, "nitty exited with");
        assert!(screen.contains("nitty exited with 0") && !screen.contains("? help"), "{screen}");
        assert!(t.alive());
        // The server goes before the folder of its files.
        drop(t);
        std::fs::remove_dir_all(&dir).ok();
    });
}
