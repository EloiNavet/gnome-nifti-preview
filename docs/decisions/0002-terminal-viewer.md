# ADR 0002 — The terminal viewer: nitty, the same library, a terminal layer written by hand

Status: accepted · Date: 2026-10-27

## Context

The preview needs GNOME. Medical images are just as often on a cluster, a
server reached over SSH or in a container, where the only display is a
terminal. Two projects show images in a terminal: niicat (Python, prints the
views with the iTerm2, Kitty or Sixel protocol, chosen by an option; nothing
appears in a terminal without that protocol, such as GNOME Terminal) and
headjack (Rust, an interactive viewer in half blocks, ratatui and crossterm).
Neither reads more than NIfTI, nor gets the orientation of an oblique or
permuted image right, nor colours segmentations, nor browses 4D images.

The library already does everything a viewer needs and draws each slice as
RGB8. What a terminal viewer adds is putting those pixels on a terminal and
handling keys.

## Decisions

* **A separate program, `nitty`**: NIfTI + TTY (`niicat` is taken). Without
  `-p` in a terminal it is the interactive viewer; with `-p`, or when its
  output is not a terminal, a quick look that prints and returns.
* **A member of a Cargo workspace rooted in `backend/`**, not a feature of the
  preview's crate: `cargo build` in `backend/` still builds only
  `nifti-preview` (default member), with the same lock file, target folder and
  release profile, so the preview's build and binary do not change (its
  installer and CI gain separate paths for nitty: `--terminal`, jobs of their
  own); `cargo install ... nitty` and `cargo build -p nitty` build the
  terminal viewer alone.
* **Nothing of the library rewritten.** What `server.rs` computed in private for
  the Sushi viewer (start cursor, readout, labels, series, cursor carried across
  modes, loading on a thread) became `view.rs`, used by both; the panel's text,
  until then only in JavaScript, has a Rust port (`panel.rs`) that `tests/gjs`
  compares with the JavaScript. The protocol is unchanged: a session of every
  request on 488 files gives byte-identical output before and after.
* **The terminal layer is written over `libc`**, not ratatui or crossterm:
  - `libc` is already in the lock file (memmap2): no new crate. ratatui 0.30
    needs Rust 1.88 (the MSRV is 1.85) and brings about 35 crates.
  - The library's sandbox refuses sockets: crossterm waits for SIGWINCH through
    a socket pair, and opens `/dev/tty` when it needs it, which Landlock refuses
    once applied. nitty opens the terminal once, before the sandbox, and takes
    signals through a pipe.
  - Speed comes from what is written to the terminal, which this layer controls:
    only the cells that changed, the fewest SGR parameters, one write per frame
    inside a synchronized update; a picture is computed again only when what it
    shows changes, and with an image protocol only that picture is sent again
    (the crosshair is marked in the margins, not drawn in the images). Measured
    on a 200 × 60 terminal, moving the cursor in a 1 mm T1 or a 512² CT: in
    half blocks 1 to 6 ms of work and 25 to 60 kB written per key; with Kitty
    or iTerm2 one view, 250 to 370 kB in 6 to 13 ms; with Sixel 400 to 790 kB
    in 20 to 30 ms.
* **Every terminal gets an image.** Half blocks in 24-bit, 256 or 16 colours
  everywhere; Kitty's, iTerm2's or the Sixel protocol where the terminal says
  it has one (its answers to a Kitty query, XTVERSION and DA1, sent together,
  DA1 last so that its answer ends the wait in one round trip); in tmux, only
  what tmux draws itself. `--protocol` and `--colors` override the choice.
* **Installed on its own**: `install.sh --terminal` and `get.sh --terminal` put
  one file in `~/.local/bin` (or `/usr/local/bin` as root, for containers); the
  two installations never touch each other's files. Releases carry a static
  (musl) build per architecture in `nitty.tar.gz`, in the same release as the
  preview: a separate tag would become `releases/latest` and hide one archive
  from `get.sh`. The static build is as fast as the glibc one (quick look of a
  1 mm T1: 97 against 92 ms; a 64 MB CT: 295 against 325 ms).

## Consequences

* The library gains public API (`view`, `panel`, `Grid::voxel_at`,
  `ViewPlan::screen_point`) that `nifti-preview` mostly does not use; the
  linker drops it from that binary.
* The terminal layer is Linux-only (`pipe2`, the sandbox), like the sandbox.
* The panel's text exists twice (JavaScript for the viewers, Rust for nitty);
  the parity test keeps them equal. Improvement left for later: the library
  writes the panel's text (`panel.rs`) and the protocol carries it, so that
  the JavaScript only shows it and the duplicate goes away. Not done here, to
  keep the protocol unchanged.
* niicat (GPL-2.0-only) gave ideas only; no code of headjack (MIT) was copied:
  its rendering is tied to ratatui.
