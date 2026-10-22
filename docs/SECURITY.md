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

**Previewer side** (GJS):
- A reply line longer than 8 MiB, a slice larger than the slice limits, a
  spectrum inconsistent with its length, an unexpected id or malformed JSON
  makes the preview stop with an error. Nothing is allocated for it.
- A backend that does not start or answer within its deadline is stopped.
- Error text is escaped before Sushi 51 renders it as markup.
- Moving quickly from file to file never leaves a backend behind.

**Installers**:
- `get.sh`, `install.sh` and `uninstall.sh` refuse to run as root. They only
  write inside your home folder, atomically.
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
sha256sum -c gnome-nifti-preview.tar.gz.sha256
# public repository: also check where and how it was built
gh attestation verify gnome-nifti-preview.tar.gz --repo EloiNavet/gnome-nifti-preview
```

## Reporting a vulnerability

Please do not open a public issue. Use *Security → Report a vulnerability* on
the GitHub repository, so that the report stays private until a fix is
released.
