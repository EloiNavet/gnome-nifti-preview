# Security

## Threat model

A `.nii` or `.nii.gz` file can come from anywhere: a download, an archive, a
shared folder, a public dataset. Selecting it in Files and pressing Space is
enough for it to be read, so every file is treated as hostile. Reading one
must never freeze the desktop, crash the previewer, or do anything but read.

The file is parsed by `nifti-preview`, a separate process started for each
preview and stopped when it closes. GNOME Sushi only receives a few small JSON
replies and RGB pixels from it.

## What protects you

**Bounded work and memory** (backend):
- The voxel data of the first volume must fit in `max-image-mb` (3 GB by
  default; [settings](SETTINGS.md)), `.nii` and `.nii.gz` alike. A
  spectroscopy (NIfTI-MRS) spectrum needs all the data to fit; when they do
  not, its maps may still show.
- The data start (`vox_offset`) must be within the first 256 MB; only the first
  MB of header extensions is read.
- A slice has at most 16 megapixels (4096 × 4096), no side above 16384. Larger
  images show their metadata only. Slices resampled from an oblique image have
  at most 16 times the pixels of the stored ones.
- Every buffer sized from the header is checked against these limits before it
  is allocated, and allocations sized by the image are fallible: if memory runs
  out, the result is an error, not a crash.
- Gzip streams are inflated only up to the end of the first volume (of all the
  data for a spectrum), whatever the file claims. Decompression bombs and gzip
  trailers that lie about their size have no effect.
- The file is opened once and checked with `fstat`. If the path is replaced by
  a FIFO or a device afterwards, nothing changes.
- Arithmetic overflows are checked, even in the release build.
- Header text is cleaned of control characters and bidirectional overrides.
- Text headers (MRtrix, NRRD) are read up to 4 MB, dimensions and data sizes
  are checked before anything is allocated. Data files named by a NRRD `.nhdr`
  or an MRtrix `.mih` (at most 16 384, in its folder: an absolute path or `..` is refused, as for a
  MetaImage `.mhd`) get the same checks as the image
  (a regular file, opened without blocking); they are only read. A symbolic link
  among them is followed only to a file in the header's folder: the file actually
  opened is checked (`/proc/self/fd`). The images a DICOMDIR lists stay in its
  folder the same way, checked when the DICOMDIR is read.
- Files on network file systems are never memory-mapped (a dropped connection
  would end the backend with SIGBUS); they are read in blocks, in the
  background, and a load given up (another file, closing) stops reading.
  Thumbnails of such files are refused unless asked for.
- DICOM: a file of a series makes the backend read the headers of the other
  files of its folder (at most 10 000), and the pixels of those of its series;
  a DICOMDIR names files below its own folder only (never `..`). Parsing is
  bounded (nesting depth, element count, value sizes, fragments) and stops at
  the pixel data. JPEG, JPEG-LS and RLE are decoded by Rust code written for
  this project; JPEG 2000 by OpenJPEG (C), inside the same sandbox, after the
  image size of the codestream was checked. NRRD's bzip2, ascii and hex data
  are decoded by Rust code written for this project too, never beyond the size
  the header declares. Decoders are fuzzed in CI.

**Sandbox** (backend, Linux, best effort on older kernels):
- Its data memory is capped a little above the image limit (`RLIMIT_DATA`;
  read-only file mappings do not count).
- It can gain no privileges (`PR_SET_NO_NEW_PRIVS`).
- It closes every descriptor it inherited but its standard input, output and
  error, and a seccomp filter makes creating a socket of any kind (TCP, UDP,
  Unix, netlink…) or an io_uring fail (x86_64 and aarch64).
- [Landlock](https://docs.kernel.org/userspace-api/landlock.html) forbids it to
  write, create, remove, truncate or execute files, to use TCP, abstract Unix
  sockets or signals to other processes. It can only read.

A memory bug in the code that parses the file, including the C library used
for gzip, therefore cannot write to your files, run a program or reach the
network. On a kernel without Landlock (before 5.13) the file restrictions are
missing; the socket filter, the memory cap and no new privileges remain.
`NIFTI_PREVIEW_DEBUG=1` prints what was applied, and the `hello` line of the
protocol reports it.

**Terminal viewer** (nitty):
- It applies the same sandbox (memory cap, no new privileges, no sockets,
  Landlock read-only) once it has opened the terminal and asked what it can
  show, before it reads any image. The terminal's descriptor, opened before,
  keeps its ioctls; signals come through a pipe, not a socket. It writes no
  file: output goes to the terminal or standard output only.
- Every character it writes passes one filter that drops C0 and C1 control
  characters, DEL and bidirectional overrides: a file name, a header field
  (`descrip`, DICOM tags, NRRD fields), a label name of a user table or a
  terminal's own answer cannot send an escape sequence (set the clipboard, the
  title, move the cursor).
- What the terminal answers is parsed with bounds (an endless sequence is
  dropped after 4 KiB); the parser is fuzzed (`fuzz/fuzz_targets/terminal.rs`,
  also a CI job).
- Images are sent in the escape codes; nitty never asks a terminal to read a
  file (Kitty's `t=f`).
- The terminal settings are put back on every exit nitty can see: a normal
  one, a panic of its own thread, SIGTERM, SIGHUP, SIGINT, SIGQUIT, and around
  Ctrl-Z. A panic the library catches on another thread leaves the terminal
  alone (the viewer goes on). After SIGKILL nothing can run: `reset` restores
  the terminal.

**Previewer side** (GJS):
- A reply line longer than 8 MiB, a slice larger than the slice limits, a
  spectrum inconsistent with its length, an unexpected id or malformed JSON
  makes the preview stop with an error. Nothing is allocated for it.
- A backend that does not start or answer within its deadline is stopped.
- Error text is escaped before Sushi 51 renders it as markup.
- Moving quickly from file to file never leaves a backend behind.

**Thumbnails**: GNOME Files runs `gnome-nifti-preview-thumbnailer thumbnail`
in its own bubblewrap sandbox (only `/usr`, the file and the output; no
network, no home folder). The program then opens its output and applies the
sandbox above before it reads the file, with a lower memory cap (1 GB of voxel
data) and a limit on the compressed data it inflates (`[thumbnails] max-file-mb`).

**Installers**:
- `get.sh`, `install.sh` and `uninstall.sh` refuse to run as root, except to
  install or remove the terminal viewer alone (`--terminal`, for containers),
  which writes only `nitty` in its folder (`~/.local/bin`, `--bin-dir`, or
  `/usr/local/bin` as root). As root, `install.sh --terminal` builds nothing
  and installs only a file that nobody but root or the user running sudo could
  have changed. `nitty --version` is asked with a time limit, no input and a
  bounded answer; `uninstall.sh --terminal` removes `nitty` only when it is a
  regular file that answers `nitty <version>`.
- For the GNOME preview they only write inside your home folder, atomically,
  with one exception that asks first:
  for thumbnails, `install.sh` copies the backend to
  `/usr/local/bin/gnome-nifti-preview-thumbnailer` with `sudo` (GNOME's sandbox
  cannot run programs from home folders). It copies again only when the backend
  changed; `uninstall.sh` offers to remove it, and removes only that recorded
  path when it is still our program.
- `uninstall.sh` deletes only the paths that `install.sh` creates, whatever its
  manifest says, and never through a symbolic link planted in its folder.
- `install.sh` never rewrites a `mimeapps.list` that is a symbolic link,
  unreadable or invalid. If it became a link after the install (dotfiles
  adopting it), `uninstall.sh` still removes our entries from the file it
  points to: otherwise they would name launchers that no longer exist.
- `uninstall.sh` puts back `mimeapps.list` byte for byte when nothing else
  changed it since the first install; otherwise it removes only our entries.
  An external edit detected during a reinstall disables exact restoration
  permanently for that installation, even if a later run changes the file again.
- A viewer is made the default application only if neither it, nor the desktop
  entry that runs it, nor a folder above them can be changed by another user
  (whether it was found in `PATH`, a desktop entry or a download folder).
- Paths written in launchers are quoted and escaped as desktop entries require.
- `get.sh` downloads over HTTPS only, without downgrade, and checks the
  archive's SHA-256 before running anything from it.

**Releases**:
- The workflow that publishes them has a write token only in the job that
  uploads the archive, and that job runs only GitHub's own actions.
- Every action is pinned to a commit.
- Dependencies are checked against the RustSec advisory database
  (`cargo deny`) on every change and every week.
- The file reader is fuzzed on every change.
- The archive is reproducible.

## Checking a download

```sh
sha256sum -c gnome-nifti-preview.tar.gz.sha256     # or nitty.tar.gz.sha256
# public repository: also check where and how it was built
gh attestation verify gnome-nifti-preview.tar.gz --repo EloiNavet/gnome-nifti-preview
```

## Reporting a vulnerability

Please do not open a public issue. Use *Security → Report a vulnerability* on
the GitHub repository, so that the report stays private until a fix is
released.
