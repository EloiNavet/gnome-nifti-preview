#!/usr/bin/env bash
# Build the release archive downloaded by get.sh.
#
#   tools/package.sh VERSION OUT_DIR ARCH=BINARY [ARCH=BINARY...]
#   e.g. tools/package.sh 0.1.0 dist x86_64=backend/target/release/nifti-preview
#   tools/package.sh --terminal VERSION OUT_DIR ARCH=NITTY [ARCH=NITTY...]
#
# Writes OUT_DIR/gnome-nifti-preview.tar.gz (one directory
# gnome-nifti-preview-VERSION/ with install.sh and bin/ARCH/nifti-preview)
# and its .sha256. With --terminal: OUT_DIR/nitty.tar.gz (nitty-VERSION/ with
# install.sh, uninstall.sh and bin/ARCH/nitty, statically linked binaries
# only). Used by .github/workflows/release.yml and the tests.
#
# Only files tracked by git are packed, and the archive is reproducible: same
# sources and binaries, same bytes (file order, dates and owners are fixed).
set -euo pipefail
unset CDPATH
ROOT="$(cd -- "$(dirname -- "$0")/.." >/dev/null && pwd)"
die() { echo "package.sh: $*" >&2; exit 1; }
terminal=0
if [ "${1:-}" = --terminal ]; then terminal=1; shift; fi
version="${1:?version}"; out="${2:?output directory}"; shift 2
(($#)) || { echo "at least one ARCH=BINARY is needed" >&2; exit 64; }
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+.][0-9A-Za-z.-]+)?$ ]] || die "invalid version: $version"
mkdir -p "$out"; out="$(cd -- "$out" >/dev/null && pwd)"
stage="$(mktemp -d)"; trap 'rm -rf -- "$stage"' EXIT
product=gnome-nifti-preview; program=nifti-preview
files=(install.sh uninstall.sh LICENSE THIRD-PARTY-NOTICES.md README.md sushi mime)
if ((terminal)); then
    product=nitty; program=nitty
    files=(install.sh uninstall.sh LICENSE THIRD-PARTY-NOTICES.md README.md)
fi
name="$product-$version"
d="$stage/$name"
mkdir -p "$d"
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
    # (Outputs kept whole: with pipefail, readelf | grep -q can fail on a
    # match, when grep stops reading first.)
    if command -v readelf >/dev/null; then
        elf="$(readelf -h "$bin" 2>&1)" || die "$bin is not an ELF file"
        grep -q "Machine:.*$machine" <<<"$elf" || die "$bin is not an $arch binary"
        # The terminal viewer runs on any Linux (a cluster, a container): no
        # dynamic loader, no shared library.
        if ((terminal)); then
            elf="$(readelf -l -d "$bin" 2>&1)" || die "readelf cannot read $bin"
            if grep -q -e INTERP -e NEEDED <<<"$elf"; then
                die "$bin is not statically linked (build it for $arch-unknown-linux-musl)"
            fi
        fi
    elif ((terminal)); then
        die "readelf (binutils) is needed to check that nitty is statically linked"
    fi
    install -D -m 755 "$bin" "$d/bin/$arch/$program"
done
tar --sort=name --mtime="@$epoch" --owner=0 --group=0 --numeric-owner --mode='u+rwX,go+rX,go-w' \
    -C "$stage" -cf - "$name" | gzip -n -9 > "$out/$product.tar.gz"
(cd "$out" && sha256sum "$product.tar.gz" > "$product.tar.gz.sha256")
echo "$out/$product.tar.gz"
