#!/usr/bin/env bash
# Every Rust crate compiled into nifti-preview and nitty (default features,
# the Linux targets the releases are built for) must be named in
# THIRD-PARTY-NOTICES.md, which both release archives carry with the licence
# texts. (Not --target all: it also lists crates under cfg(any()), which are
# never compiled.)
#
#   tools/check-notices.sh
#
# Run by CI. A crate added to (or replacing one in) the dependency graph
# fails it until its notice is written.
set -euo pipefail
unset CDPATH
ROOT="$(cd -- "$(dirname -- "$0")/.." >/dev/null && pwd)"
notices="$ROOT/THIRD-PARTY-NOTICES.md"
targets=()
for t in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
    targets+=(--target "$t")
done
crates="$(cd "$ROOT/backend" && cargo tree --locked -e normal "${targets[@]}" --prefix none --format '{p}' \
    -p gnome-nifti-preview -p nitty | awk '{print $1}' | sort -u)"
[ -n "$crates" ] || { echo "check-notices: cargo tree listed no crate" >&2; exit 1; }
missing=()
for c in $crates; do
    case "$c" in gnome-nifti-preview | nitty) continue ;; esac
    grep -qF -- "| \`$c\` |" "$notices" || missing+=("$c")
done
if ((${#missing[@]})); then
    echo "THIRD-PARTY-NOTICES.md has no notice for: ${missing[*]}" >&2
    exit 1
fi
echo "THIRD-PARTY-NOTICES.md names the $(wc -w <<<"$crates") crates compiled in (the two programs included)"
