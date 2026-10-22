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

`install.sh` picks the newest `cargo` / `cargo-1.NN` it finds, or takes a prebuilt
backend with `--binary PATH`. Running it again upgrades in place.
When building from source, the installer reads Cargo's executable artifact path,
so `CARGO_TARGET_DIR` and Cargo's configured target directory are respected.

## Layout

```
backend/     Rust backend (nifti-preview): NIfTI reader, orientation, rendering, protocol
sushi/       GJS viewers: legacy/ (Sushi 46–50, GTK 3), v1/ (Sushi 51+, GTK 4), common/
mime/        shared-mime-info definitions for .nii and .nii.gz
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
cargo test && cargo test --release
cargo clippy --all-targets -- -D warnings
cargo deny check                                   # cargo install --locked cargo-deny
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
provenance (`gh attestation verify`). The one-command install works once the
repository is public and a release exists.
