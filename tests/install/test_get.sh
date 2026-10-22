#!/usr/bin/env bash
# One-command install (get.sh) against a local copy of a release.
#
#   tests/install/test_get.sh [BACKEND_BINARY]
#
# Builds the release archive with tools/package.sh, serves it over HTTP on
# localhost and runs get.sh the way the README tells people to: through a
# pipe, in a fresh home directory, with curl and with wget only.
set -euo pipefail
unset CDPATH
ROOT="$(cd -- "$(dirname -- "$0")/../.." >/dev/null && pwd)"
BIN="$(realpath "${1:-$ROOT/backend/target/release/nifti-preview}")"
. "$ROOT/tests/lib/home.sh"
WORK="$(mktemp -d)"
SERVER=""
trap '[ -z "$SERVER" ] || kill "$SERVER" 2>/dev/null; rm -rf -- "$WORK"' EXIT
failures=0
ok() { printf '  ok    %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; ci_error "$1"; failures=$((failures + 1)); }

"$ROOT/tools/package.sh" 0.0.0-test "$WORK/release" "$(uname -m)=$BIN" >/dev/null
free_port() { python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])'; }
serve() { # directory port: start a static server and wait until it answers
    python3 -m http.server "$2" --bind 127.0.0.1 --directory "$1" >/dev/null 2>&1 &
    for _ in $(seq 100); do python3 -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:$2/')" 2>/dev/null && return 0; sleep 0.1; done
    echo "server on port $2 did not start"; exit 1
}
port=$(free_port)
serve "$WORK/release" "$port"
SERVER=$!
export GNOME_NIFTI_PREVIEW_URL="http://127.0.0.1:$port"
export GNOME_NIFTI_PREVIEW_SERVICE_DIRS=/usr/share   # this machine has Sushi

# A PATH without curl, to exercise the wget download.
mkdir -p "$WORK/nocurl"
for f in /usr/bin/* /usr/sbin/*; do
    [ "$(basename "$f")" = curl ] || ln -sf "$f" "$WORK/nocurl/$(basename "$f")"
done

snapshot() { (cd "$1" && find . -path ./.cache -prune -o -print | sort); }
fresh_home() {
    use_home "$(mktemp -d "$WORK/home.XXXXXX")"
    snapshot "$HOME" > "$HOME.before"
}
installed() {
    [ -x "$HOME/.local/share/gnome-nifti-preview/bin/nifti-preview" ] &&
        [ -x "$HOME/.local/share/gnome-nifti-preview/uninstall.sh" ] &&
        [ -f "$HOME/.local/share/sushi/viewers/niftiPreview.js" ] &&
        [ -f "$HOME/.local/share/sushi/plugins-1/niftiPreview.js" ] &&
        [ -f "$HOME/.local/share/mime/packages/gnome-nifti-preview.xml" ] &&
        "$HOME/.local/share/gnome-nifti-preview/bin/nifti-preview" --version >/dev/null
}
restored() { snapshot "$HOME" | diff -q - "$HOME.before" >/dev/null; }

echo "== curl"
command -v curl >/dev/null || { echo "curl is needed for this test"; exit 1; }
fresh_home
out="$(bash < "$ROOT/get.sh" 2>&1)" && installed && ok "installed through a pipe" || { fail "install"; echo "$out"; }
grep -q 'press Space' <<<"$out" && ok "tells what to do next" || fail "final message"
[ -z "$(find "$HOME/.cache" -maxdepth 1 -name 'gnome-nifti-preview-get.*' 2>/dev/null)" ] \
    && ok "download directory removed" || fail "download directory left behind"
bash < "$ROOT/get.sh" >/dev/null 2>&1 && installed && ok "running it again upgrades in place" || fail "reinstall"
bash -s -- --uninstall < "$ROOT/get.sh" >/dev/null 2>&1 && restored && ok "--uninstall restores the home directory" || fail "--uninstall"

echo "== wget only"
command -v wget >/dev/null || { echo "wget is needed for this test"; exit 1; }
fresh_home
PATH="$WORK/nocurl" bash < "$ROOT/get.sh" >/dev/null 2>&1 && installed && ok "installed with wget" || fail "wget install"
"$HOME/.local/share/gnome-nifti-preview/uninstall.sh" >/dev/null && restored && ok "the copied uninstall.sh restores the home directory" || fail "uninstall.sh"

echo "== failures leave nothing behind"
fresh_home
cp -r "$WORK/release" "$WORK/bad"
echo "0000000000000000000000000000000000000000000000000000000000000000  gnome-nifti-preview.tar.gz" > "$WORK/bad/gnome-nifti-preview.tar.gz.sha256"
bad_port=$(free_port)
serve "$WORK/bad" "$bad_port"
BAD=$!
out="$(GNOME_NIFTI_PREVIEW_URL="http://127.0.0.1:$bad_port" bash < "$ROOT/get.sh" 2>&1)" && fail "corrupted archive accepted" ||
    { grep -q checksum <<<"$out" && restored && ok "corrupted download refused, nothing installed" || { fail "corrupted download"; echo "$out"; }; }
kill "$BAD" 2>/dev/null || true
out="$(GNOME_NIFTI_PREVIEW_URL="http://127.0.0.1:1" bash < "$ROOT/get.sh" 2>&1)" && fail "unreachable server accepted" ||
    { grep -q 'download failed' <<<"$out" && restored && ok "network error reported, nothing installed" || { fail "network error"; echo "$out"; }; }

echo "== hostile downloads and environments"
# A .sha256 that names another file (here /dev/null, whose hash it gives) is refused.
cp -r "$WORK/release" "$WORK/devnull"
echo "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  /dev/null" > "$WORK/devnull/gnome-nifti-preview.tar.gz.sha256"
dn_port=$(free_port)
serve "$WORK/devnull" "$dn_port"
DN=$!
fresh_home
out="$(GNOME_NIFTI_PREVIEW_URL="http://127.0.0.1:$dn_port" bash < "$ROOT/get.sh" 2>&1)" && fail "checksum of another file accepted" ||
    { grep -q checksum <<<"$out" && restored && ok "a checksum file naming another file is refused" || { fail ".sha256 for /dev/null"; echo "$out"; }; }
kill "$DN" 2>/dev/null || true
# Nothing runs from /tmp (which may be mounted noexec).
fresh_home
out="$(TMPDIR=/nonexistent/tmp bash < "$ROOT/get.sh" 2>&1)" && installed && ok "works without a usable /tmp" || { fail "TMPDIR"; echo "$out"; }
grep -q "Note: downloading from http://127.0.0.1" <<<"$out" && ok "a download source override is announced" || fail "override not announced"
bash -s -- --uninstall < "$ROOT/get.sh" >/dev/null 2>&1
# Never as root (wget ... | sudo bash).
mkdir -p "$WORK/fakeid"
printf '#!/bin/sh\n[ "$1" = -u ] && { echo 0; exit 0; }\nexec /usr/bin/id "$@"\n' > "$WORK/fakeid/id"; chmod +x "$WORK/fakeid/id"
fresh_home
out="$(env -u GNOME_NIFTI_PREVIEW_ALLOW_ROOT PATH="$WORK/fakeid:$PATH" bash < "$ROOT/get.sh" 2>&1)" && fail "ran as root" ||
    { grep -q 'do not run this with sudo' <<<"$out" && restored && ok "refuses to run as root" || { fail "root"; echo "$out"; }; }

echo "== GNOME Sushi missing"
out="$(GNOME_NIFTI_PREVIEW_SERVICE_DIRS=/nonexistent setsid bash < "$ROOT/get.sh" 2>&1)" && fail "installed without Sushi" ||
    { grep -q 'GNOME Sushi is required. Install it with: sudo ' <<<"$out" && restored && ok "without a terminal: prints the install command, installs nothing" || { fail "no tty"; echo "$out"; }; }
if command -v script >/dev/null; then
    out="$(printf 'n\n' | GNOME_NIFTI_PREVIEW_SERVICE_DIRS=/nonexistent script -qec "bash $(printf %q "$ROOT/get.sh")" /dev/null 2>&1)" && fail "answer no ignored" ||
        { grep -q 'Install it now' <<<"$out" && grep -q 'GNOME Sushi is required' <<<"$out" && restored && ok "asks before installing Sushi; answering no stops cleanly" || { fail "prompt"; echo "$out"; }; }
fi

((failures)) && { echo "$failures failure(s)"; exit 1; }
echo "all get.sh checks passed"
