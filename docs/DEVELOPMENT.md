# Development

## Build from source

Needs Rust 1.85 or later.

```sh
sudo apt install gnome-sushi cargo build-essential     # Ubuntu 24.04: cargo-1.85 instead of cargo
git clone https://github.com/EloiNavet/gnome-nifti-preview.git
cd gnome-nifti-preview
./install.sh            # builds the backend, installs into ~/.local/share
./uninstall.sh          # removes everything again
```

`build-essential` provides the C compiler that builds the two vendored C
libraries, libdeflate (gzip) and OpenJPEG (JPEG 2000 in DICOM files);
`cargo build --no-default-features` builds a pure-Rust backend without them
(slower gzip, JPEG 2000 refused).

`install.sh` picks the newest `cargo` / `cargo-1.NN` it finds, or takes a prebuilt
backend with `--binary PATH`. Running it again upgrades in place.
When building from source, the installer reads Cargo's executable artifact path,
so `CARGO_TARGET_DIR` and Cargo's configured target directory are respected.

The terminal viewer needs only Rust (and `build-essential` for the same two C
libraries). From the repository's root:

```sh
./install.sh --terminal                 # builds nitty, installs ~/.local/bin/nitty
./uninstall.sh --terminal
cargo build --manifest-path backend/Cargo.toml -p nitty --release   # or just build it
cargo install --locked --path backend/terminal                      # or install it with cargo
```

`install.sh --terminal` builds for this machine's target, named explicitly,
into `backend/target/<target>/release/nitty`, whatever `CARGO_TARGET_DIR` or a
configured `build.target` say, and installs that file. As root it does not
build (it would leave files of root in the working copy): build as yourself,
then `sudo ./install.sh --terminal --binary PATH`.

A static binary, as releases ship it (`sudo apt install musl-tools`):

```sh
rustup target add x86_64-unknown-linux-musl
cd backend && cargo build -p nitty --release --locked --target x86_64-unknown-linux-musl
```

## Layout

```
backend/     Rust backend (nifti-preview): NIfTI, MGH, MRtrix, NRRD and DICOM readers, orientation, rendering, protocol;
             a Cargo workspace whose default member is the backend
backend/terminal/  the terminal viewer (nitty), on the same library
sushi/       GJS viewers: legacy/ (Sushi 46–50, GTK 3), v1/ (Sushi 51+, GTK 4), common/
mime/        shared-mime-info definitions for NIfTI, MGH/MGZ, MRtrix and NRRD (DICOM's is shared-mime-info's own)
reference/   nibabel implementation: test oracle and synthetic fixture generator
tests/       GJS, MIME, install, one-command install and real-Sushi (Xvfb) tests
tools/       release packaging
get.sh       one-command installer (downloads the latest release)
```

How the parts fit: [ARCHITECTURE.md](ARCHITECTURE.md). Backend protocol:
[PROTOCOL.md](PROTOCOL.md). Tests and how to run them: [TESTING.md](TESTING.md).

## Checks

```sh
cd backend
cargo test --workspace && cargo test --workspace --release
cargo clippy --workspace --all-targets -- -D warnings
cargo deny --workspace check                       # cargo install --locked cargo-deny
python3 fuzz/make_seeds.py /tmp/corpus             # cargo install --locked cargo-fuzz
cargo +nightly fuzz run open_load_render /tmp/corpus -- -max_total_time=600
```

The other suites are listed in [TESTING.md](TESTING.md). Security design:
[SECURITY.md](SECURITY.md).

## Releasing

Push a tag such as `v0.1.0` (a tag with a suffix, `v0.2.0-rc1`, makes a
pre-release, which `get.sh` does not pick). `.github/workflows/release.yml` builds
the backend for x86_64 and aarch64 (on Ubuntu 22.04, so it runs on any newer
glibc), packs it with `tools/package.sh` (reproducible archive) and publishes
`gnome-nifti-preview.tar.gz` and its `.sha256`, which `get.sh` downloads from
`releases/latest`; on a public repository it also records the build
provenance (`gh attestation verify`). The same release carries `nitty.tar.gz`,
the terminal viewer built statically (musl) for each architecture
(`tools/package.sh --terminal`), which `get.sh --terminal` downloads. The one-command install works once the
repository is public and a release exists.

The tag must name the version of the sources (`backend/Cargo.toml` and
`backend/terminal/Cargo.toml`; `v0.3.0-rc1` is a pre-release of 0.3.0), or the
release stops before publishing. Before tagging, run the same workflow by hand
(Actions > Release > Run workflow, or `gh workflow run release.yml --ref main`):
it builds and tests both programs for each architecture (aarch64 on a public
repository only), assembles both archives and keeps them as the run's
artifacts, without publishing anything.
