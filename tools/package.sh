#!/usr/bin/env bash
# Build the release archive downloaded by get.sh.
#
#   tools/package.sh VERSION OUT_DIR ARCH=BINARY [ARCH=BINARY...]
#   e.g. tools/package.sh 0.1.0 dist x86_64=backend/target/release/nifti-preview
#
# Writes OUT_DIR/gnome-nifti-preview.tar.gz (one directory
# gnome-nifti-preview-VERSION/ with install.sh and bin/ARCH/nifti-preview)
# and its .sha256. Used by .github/workflows/release.yml and the tests.
#
# Only files tracked by git are packed, and the archive is reproducible: same
# sources and binaries, same bytes (file order, dates and owners are fixed).
set -euo pipefail
unset CDPATH
ROOT="$(cd -- "$(dirname -- "$0")/.." >/dev/null && pwd)"
die() { echo "package.sh: $*" >&2; exit 1; }
version="${1:?version}"; out="${2:?output directory}"; shift 2
(($#)) || { echo "at least one ARCH=BINARY is needed" >&2; exit 64; }
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+.][0-9A-Za-z.-]+)?$ ]] || die "invalid version: $version"
mkdir -p "$out"; out="$(cd -- "$out" >/dev/null && pwd)"
stage="$(mktemp -d)"; trap 'rm -rf -- "$stage"' EXIT
name="gnome-nifti-preview-$version"
d="$stage/$name"
mkdir -p "$d"
files=(install.sh uninstall.sh LICENSE README.md sushi mime)
if git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    git -C "$ROOT" ls-files -z -- "${files[@]}" | (cd "$ROOT" && tar --null -T - -cf -) | tar -xf - -C "$d"
    epoch="${SOURCE_DATE_EPOCH:-$(git -C "$ROOT" log -1 --format=%ct)}"
else
    echo "package.sh: not a git checkout, packing the files as they are" >&2
    (cd "$ROOT" && tar -cf - "${files[@]}") | tar -xf - -C "$d"
    epoch="${SOURCE_DATE_EPOCH:-0}"
fi
for pair in "$@"; do
    arch="${pair%%=*}"; bin="${pair#*=}"
    case "$arch" in
        x86_64) machine="X86-64" ;;
        aarch64) machine="AArch64" ;;
        *) die "unsupported architecture: $arch" ;;
    esac
    [ -f "$bin" ] && [ -x "$bin" ] || die "$bin is not an executable"
    if command -v readelf >/dev/null; then
        readelf -h "$bin" | grep -q "Machine:.*$machine" || die "$bin is not an $arch binary"
    fi
    install -D -m 755 "$bin" "$d/bin/$arch/nifti-preview"
done
tar --sort=name --mtime="@$epoch" --owner=0 --group=0 --numeric-owner --mode='u+rwX,go+rX,go-w' \
    -C "$stage" -cf - "$name" | gzip -n -9 > "$out/gnome-nifti-preview.tar.gz"
(cd "$out" && sha256sum gnome-nifti-preview.tar.gz > gnome-nifti-preview.tar.gz.sha256)
echo "$out/gnome-nifti-preview.tar.gz"
