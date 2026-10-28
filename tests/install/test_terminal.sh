#!/usr/bin/env bash
# The terminal viewer's installation, independent of the GNOME preview:
# install.sh --terminal, uninstall.sh --terminal, get.sh --terminal from a
# local copy of a release, and neither removal touching the other.
#
#   tests/install/test_terminal.sh STATIC_NITTY [NIFTI_PREVIEW]
#
# STATIC_NITTY is a statically linked nitty (the release build, musl).
# NIFTI_PREVIEW (dynamically linked) is the binary the static check must
# refuse, and the GNOME checks run with it when gjs is installed. Root is
# simulated (an `id` that answers 0); nothing here needs privileges.
set -euo pipefail
unset CDPATH
ROOT="$(cd -- "$(dirname -- "$0")/../.." >/dev/null && pwd)"
NITTY="$(realpath "${1:?usage: test_terminal.sh STATIC_NITTY [NIFTI_PREVIEW]}")"
PREVIEW="${2:+$(realpath "$2")}"
. "$ROOT/tests/lib/home.sh"
WORK="$(mktemp -d)"
SERVER=""
trap '[ -z "$SERVER" ] || kill "$SERVER" 2>/dev/null; rm -rf -- "$WORK"' EXIT
failures=0
ok() { printf '  ok    %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; ci_error "$1"; failures=$((failures + 1)); }
snapshot() { (cd "$HOME" && find . -mindepth 1 -path ./.cache -prune -o -print | sort); }
fresh_home() {
    use_home "$(mktemp -d "$WORK/home.XXXXXX")"
    unset XDG_BIN_HOME
    snapshot > "$HOME.before"
}
restored() { snapshot | diff -q - "$HOME.before" >/dev/null; }
# Removing nitty leaves the folders it went into (.local/bin may be in PATH).
cleaned() { [ ! -e "$HOME/.local/bin/nitty" ] && [ "$(snapshot | diff - "$HOME.before" | grep '^<' | tr -d '\n')" = "< ./.local< ./.local/bin" ]; }

# A system without GNOME: a PATH without gjs, update-mime-database or gio.
mkdir -p "$WORK/nognome"
for f in /usr/bin/* /bin/*; do
    case "$(basename "$f")" in gjs*|update-mime-database|gio|gnome-*) continue ;; esac
    [ -e "$WORK/nognome/$(basename "$f")" ] || [ -L "$WORK/nognome/$(basename "$f")" ] || ln -s "$f" "$WORK/nognome/$(basename "$f")"
done
NOGNOME="$WORK/nognome"

echo "== install.sh --terminal, without GNOME"
fresh_home
out="$(PATH="$NOGNOME" "$ROOT/install.sh" --terminal --binary "$NITTY" 2>&1)" || { fail "install"; echo "$out"; }
[ -x "$HOME/.local/bin/nitty" ] && [ "$("$HOME/.local/bin/nitty" --version)" = "$("$NITTY" --version)" ] && ok "nitty installed in .local/bin and runs" || fail "not installed"
[ "$(snapshot | diff - "$HOME.before" | grep -c '^<')" = 3 ] && ok "nothing else in the home folder (.local, .local/bin, nitty)" || { fail "other files"; snapshot | diff - "$HOME.before"; }
grep -q 'not in your PATH' <<<"$out" && ok "says when the folder is not in PATH" || fail "no PATH hint"
out="$(PATH="$HOME/.local/bin:$NOGNOME" "$ROOT/install.sh" --terminal --binary "$NITTY" 2>&1)"
grep -q 'Try it' <<<"$out" && ! grep -q 'not in your PATH' <<<"$out" && ok "reinstall in place; no hint when in PATH" || { fail "reinstall"; echo "$out"; }
compgen -G "$HOME/.local/bin/.gnome-nifti-preview*" >/dev/null && fail "temporary file left" || ok "no temporary file"
PATH="$NOGNOME" "$ROOT/uninstall.sh" --terminal --quiet && cleaned && ok "uninstall.sh --terminal removes it (the folders stay)" || fail "uninstall"
out="$(PATH="$NOGNOME" "$ROOT/uninstall.sh" --terminal 2>&1)" && grep -q 'not installed' <<<"$out" && ok "uninstalling twice is harmless" || fail "second uninstall"

echo "== folders and refusals"
fresh_home
XDG_BIN_HOME="$HOME/bin" PATH="$NOGNOME" "$ROOT/install.sh" --terminal --binary "$NITTY" --quiet 2>/dev/null
[ -x "$HOME/bin/nitty" ] && ok "XDG_BIN_HOME respected" || fail "XDG_BIN_HOME"
XDG_BIN_HOME="$HOME/bin" PATH="$NOGNOME" "$ROOT/uninstall.sh" --terminal --quiet && [ ! -e "$HOME/bin/nitty" ] && ok "and uninstalled from there" || fail "XDG_BIN_HOME uninstall"
PATH="$NOGNOME" "$ROOT/install.sh" --terminal --binary "$NITTY" --bin-dir "$WORK/opt bin" --quiet 2>/dev/null
[ -x "$WORK/opt bin/nitty" ] && ok "--bin-dir (with a space)" || fail "--bin-dir"
PATH="$NOGNOME" "$ROOT/uninstall.sh" --terminal --bin-dir "$WORK/opt bin" --quiet && [ ! -e "$WORK/opt bin/nitty" ] && ok "--bin-dir uninstall" || fail "--bin-dir uninstall"
# A slash at the end names the same folder (PATH is compared without it);
# --terminal may come after the other options.
out="$(PATH="$WORK/opt bin:$NOGNOME" "$ROOT/install.sh" --quiet --binary "$NITTY" --bin-dir "$WORK/opt bin/" --terminal 2>&1)" &&
    [ -x "$WORK/opt bin/nitty" ] && ! grep -q 'not in your PATH' <<<"$out" && ok "--bin-dir with a slash at the end; --terminal last" || { fail "--bin-dir slash"; echo "$out"; }
PATH="$NOGNOME" "$ROOT/uninstall.sh" --quiet --bin-dir "$WORK/opt bin/" --terminal && [ ! -e "$WORK/opt bin/nitty" ] && ok "uninstall.sh: the same, --terminal last" || fail "uninstall slash/order"
out="$(PATH="$NOGNOME" "$ROOT/install.sh" --binary "$NITTY" --bin-dir "$WORK/x" 2>&1)" && fail "--bin-dir without --terminal accepted" ||
    { grep -q 'option of --terminal' <<<"$out" && ok "--bin-dir without --terminal refused" || { fail "--bin-dir message"; echo "$out"; }; }
# HOME matters only for the default folder.
out="$(env -u HOME PATH="$NOGNOME" "$ROOT/install.sh" --terminal --binary "$NITTY" --bin-dir "$WORK/nohome" --quiet 2>&1)" &&
    [ -x "$WORK/nohome/nitty" ] && ok "no HOME needed with --bin-dir" || { fail "HOME with --bin-dir"; echo "$out"; }
out="$(env -u HOME -u XDG_BIN_HOME PATH="$NOGNOME" "$ROOT/install.sh" --terminal --binary "$NITTY" 2>&1)" && fail "no HOME accepted" ||
    { grep -q 'HOME is not set' <<<"$out" && ok "no HOME and no --bin-dir: refused" || { fail "HOME message"; echo "$out"; }; }
"$ROOT/uninstall.sh" -h | grep -q -- '--terminal \[--bin-dir' && "$ROOT/install.sh" -h | grep -q -- '--terminal \[--binary' && ok "-h of both scripts tells of --terminal" || fail "-h"
out="$(PATH="$NOGNOME" "$ROOT/install.sh" --terminal --binary "$NITTY" --bin-dir relative/bin 2>&1)" && fail "relative --bin-dir accepted" || { grep -q 'absolute' <<<"$out" && ok "relative --bin-dir refused" || fail "relative message"; }
printf '#!/bin/sh\necho "not-nitty 1.0"\n' > "$WORK/impostor"; chmod +x "$WORK/impostor"
out="$(PATH="$NOGNOME" "$ROOT/install.sh" --terminal --binary "$WORK/impostor" 2>&1)" && fail "another program installed" || { grep -q 'is not nitty' <<<"$out" && ok "a binary that is not nitty is refused" || { fail "impostor"; echo "$out"; }; }
printf '#!/bin/sh\nprintf "\\033]0;pwned\\007\\033[2Jevil 1.0\\n"\n' > "$WORK/noisy"; chmod +x "$WORK/noisy"
out="$(PATH="$NOGNOME" "$ROOT/install.sh" --terminal --binary "$WORK/noisy" --bin-dir "$WORK/noisydir" 2>&1)" && fail "a noisy binary installed" ||
    { ! grep -q $'\e' <<<"$out" && grep -q 'evil 1.0' <<<"$out" && ok "what a refused binary says is shown without its escape sequences" || { fail "noisy"; echo "$out" | od -c | head; }; }
printf '#!/bin/sh\nexec sleep 60\n' > "$WORK/stuck"; chmod +x "$WORK/stuck"
t0=$SECONDS
out="$(PATH="$NOGNOME" "$ROOT/install.sh" --terminal --binary "$WORK/stuck" --bin-dir "$WORK/stuckdir" 2>&1)" && fail "a binary that never answers installed" ||
    { ((SECONDS - t0 < 30)) && grep -q 'does not run' <<<"$out" && ! compgen -G "$WORK/stuckdir/*" >/dev/null && ! compgen -G "$WORK/stuckdir/.g*" >/dev/null &&
        ok "a binary that never answers: refused within seconds, nothing left" || { fail "stuck binary"; echo "$out"; }; }
out="$(PATH="$NOGNOME" "$ROOT/install.sh" --terminal --thumbnails --binary "$NITTY" 2>&1)" && fail "GNOME option accepted" || { grep -q 'option of the GNOME preview' <<<"$out" && ok "GNOME options refused with --terminal" || fail "GNOME option message"; }
mkdir -p "$HOME/.local/bin" && cp "$WORK/impostor" "$HOME/.local/bin/nitty"
out="$(PATH="$NOGNOME" "$ROOT/uninstall.sh" --terminal 2>&1)" && fail "a foreign nitty removed" || { [ -e "$HOME/.local/bin/nitty" ] && ok "another program called nitty is left alone" || fail "foreign file removed"; }
rm -f "$HOME/.local/bin/nitty"
# Links: where nitty goes, a dangling link is replaced by the file (nothing
# written through it); uninstall leaves any link alone; --binary may be one.
mkdir -p "$WORK/links"
ln -s "$WORK/nowhere" "$WORK/links/nitty"
PATH="$NOGNOME" "$ROOT/install.sh" --terminal --binary "$NITTY" --bin-dir "$WORK/links" --quiet 2>/dev/null &&
    [ -f "$WORK/links/nitty" ] && [ ! -L "$WORK/links/nitty" ] && [ ! -e "$WORK/nowhere" ] && ok "a dangling link in place: replaced, nothing written through it" || fail "dangling link install"
rm -f "$WORK/links/nitty"; ln -s "$NITTY" "$WORK/links/nitty"
out="$(PATH="$NOGNOME" "$ROOT/uninstall.sh" --terminal --bin-dir "$WORK/links" 2>&1)" && fail "a link removed" ||
    { [ -L "$WORK/links/nitty" ] && grep -q 'not a regular file' <<<"$out" && ok "a link named nitty is left alone" || { fail "link uninstall"; echo "$out"; }; }
rm -f "$WORK/links/nitty"; ln -s "$WORK/nowhere" "$WORK/links/nitty"
out="$(PATH="$NOGNOME" "$ROOT/uninstall.sh" --terminal --bin-dir "$WORK/links" 2>&1)" && fail "a dangling link removed" ||
    { [ -L "$WORK/links/nitty" ] && ok "a dangling link named nitty is left alone" || { fail "dangling uninstall"; echo "$out"; }; }
ln -s "$NITTY" "$WORK/nitty-link"
PATH="$NOGNOME" "$ROOT/install.sh" --terminal --binary "$WORK/nitty-link" --bin-dir "$WORK/vialink" --quiet 2>/dev/null &&
    [ -f "$WORK/vialink/nitty" ] && [ ! -L "$WORK/vialink/nitty" ] && cmp -s "$WORK/vialink/nitty" "$NITTY" && ok "--binary given as a link: its file installed" || fail "binary link"

echo "== as root (simulated)"
mkdir -p "$WORK/fakeid"
printf '#!/bin/sh\n[ "$1" = -u ] && { echo 0; exit 0; }\nexec /usr/bin/id "$@"\n' > "$WORK/fakeid/id"; chmod +x "$WORK/fakeid/id"
as_root() { env -u GNOME_NIFTI_PREVIEW_ALLOW_ROOT PATH="$WORK/fakeid:$NOGNOME" "$@"; }
me="$(id -u)"
# A copy with known modes (the one given may sit in a folder others can write).
mkdir -p "$WORK/rootbin" && chmod 755 "$WORK/rootbin" && install -m 755 "$NITTY" "$WORK/rootbin/nitty"
as_root env -u HOME SUDO_UID="$me" "$ROOT/install.sh" --terminal --binary "$WORK/rootbin/nitty" --bin-dir "$WORK/sys" --quiet 2>/dev/null &&
    [ -x "$WORK/sys/nitty" ] && ok "as root: installed (--terminal), HOME not needed" || fail "root --terminal"
if [ "$me" != 0 ]; then
    out="$(as_root env -u SUDO_UID "$ROOT/install.sh" --terminal --binary "$WORK/rootbin/nitty" --bin-dir "$WORK/sys2" 2>&1)" && fail "root ran another user's binary" ||
        { grep -q 'belongs to another user' <<<"$out" && [ ! -e "$WORK/sys2/nitty" ] && ok "as root: a binary of another user than the one running sudo is refused" || { fail "root owner"; echo "$out"; }; }
fi
install -m 757 "$NITTY" "$WORK/rootbin/open"
out="$(as_root env SUDO_UID="$me" "$ROOT/install.sh" --terminal --binary "$WORK/rootbin/open" --bin-dir "$WORK/sys3" 2>&1)" && fail "root ran a binary others can change" ||
    { grep -q 'can be changed by other users' <<<"$out" && ok "as root: a binary others can change is refused" || { fail "root mode"; echo "$out"; }; }
mkdir -p "$WORK/opendir" && chmod 777 "$WORK/opendir" && install -m 755 "$NITTY" "$WORK/opendir/nitty"
out="$(as_root env SUDO_UID="$me" "$ROOT/install.sh" --terminal --binary "$WORK/opendir/nitty" --bin-dir "$WORK/sys4" 2>&1)" && fail "root ran a binary from a folder others can change" ||
    { grep -q 'can be changed by other users' <<<"$out" && ok "as root: nor from a folder others can change" || { fail "root folder"; echo "$out"; }; }
chmod 1777 "$WORK/opendir"
as_root env SUDO_UID="$me" "$ROOT/install.sh" --terminal --binary "$WORK/opendir/nitty" --bin-dir "$WORK/sys5" --quiet 2>/dev/null &&
    [ -x "$WORK/sys5/nitty" ] && ok "as root: from a sticky folder (like /tmp), one's own file is accepted" || fail "root sticky"
out="$(as_root "$ROOT/install.sh" --terminal --bin-dir "$WORK/sys6" 2>&1)" && fail "root built nitty" ||
    { grep -q 'not built' <<<"$out" && grep -q -- '--binary' <<<"$out" && ok "as root: nitty is not built, --binary suggested" || { fail "root build"; echo "$out"; }; }
as_root env SUDO_UID="$me" "$ROOT/uninstall.sh" --terminal --bin-dir "$WORK/sys" --quiet && [ ! -e "$WORK/sys/nitty" ] && ok "as root: uninstalled" || fail "root uninstall"
out="$(as_root "$ROOT/install.sh" --binary "$NITTY" 2>&1)" && fail "GNOME install as root" ||
    { grep -q 'do not run this with sudo' <<<"$out" && ok "as root: the GNOME preview is still refused" || fail "root GNOME"; }

echo "== building nitty: the binary is taken where it was built"
# A copy of install.sh in a source tree, a cargo and a rustc that pretend:
# cargo copies nitty where --target puts it; a stale build lies where an
# older install.sh looked. (Version 9.99: chosen over any real cargo-1.x.)
src="$WORK/src"
mkdir -p "$src/backend/terminal" "$src/backend/target/release" "$WORK/fakecargo"
cp "$ROOT/install.sh" "$src/"
: > "$src/backend/terminal/Cargo.toml"
cp "$WORK/impostor" "$src/backend/target/release/nitty"
host="$(uname -m)-unknown-linux-gnu"
{
    echo '#!/bin/sh'
    echo '[ "$1" = --version ] && { echo "cargo 9.99.0 (fake)"; exit 0; }'
    echo "echo \"\$* CARGO_TARGET_DIR=\$CARGO_TARGET_DIR\" > '$WORK/cargo-args'"
    echo 't=""; prev=""'
    echo 'for a in "$@"; do [ "$prev" = --target ] && t="$a"; prev="$a"; done'
    echo "mkdir -p \"\$CARGO_TARGET_DIR/\$t/release\" && cp '$NITTY' \"\$CARGO_TARGET_DIR/\$t/release/nitty\""
} > "$WORK/fakecargo/cargo"
printf '#!/bin/sh\nprintf "rustc 9.99.0 (fake)\\nhost: %s\\n"\n' "$host" > "$WORK/fakecargo/rustc"
chmod +x "$WORK/fakecargo/cargo" "$WORK/fakecargo/rustc"
out="$(env -u RUSTC CARGO_TARGET_DIR=/elsewhere PATH="$WORK/fakecargo:$NOGNOME" "$src/install.sh" --terminal --bin-dir "$WORK/built" 2>&1)" &&
    cmp -s "$WORK/built/nitty" "$NITTY" && grep -q -- "--target $host CARGO_TARGET_DIR=$src/backend/target\$" "$WORK/cargo-args" &&
    ok "built for this machine's target (whatever build.target or CARGO_TARGET_DIR say), that binary installed" ||
    { fail "build path"; echo "$out"; cat "$WORK/cargo-args"; }

echo "== the release archive and get.sh --terminal"
"$ROOT/tools/package.sh" --terminal 0.0.0-test "$WORK/release" "$(uname -m)=$NITTY" >/dev/null
tar -tzf "$WORK/release/nitty.tar.gz" > "$WORK/list"
grep -q "^nitty-0.0.0-test/bin/$(uname -m)/nitty$" "$WORK/list" && grep -q "^nitty-0.0.0-test/install.sh$" "$WORK/list" && ok "nitty.tar.gz holds install.sh and bin/ARCH/nitty" || fail "archive layout"
! grep -q -e sushi/ -e mime/ "$WORK/list" && ok "and nothing of the GNOME preview" || fail "GNOME files in the archive"
dynamic="${PREVIEW:-$ROOT/backend/target/release/nifti-preview}"
if [ -x "$dynamic" ]; then
    out="$("$ROOT/tools/package.sh" --terminal 0.0.0-test "$WORK/dyn" "$(uname -m)=$dynamic" 2>&1)" && fail "dynamic binary packed" ||
        { grep -q 'statically' <<<"$out" && ok "a dynamically linked binary is refused" || { fail "static check"; echo "$out"; }; }
else
    echo "  (skipped: no dynamically linked binary to refuse; give NIFTI_PREVIEW)"
fi
mkdir -p "$WORK/noreadelf" && cp -P "$NOGNOME"/* "$WORK/noreadelf/" && rm -f "$WORK/noreadelf/readelf"
out="$(PATH="$WORK/noreadelf" "$ROOT/tools/package.sh" --terminal 0.0.0-test "$WORK/norl" "$(uname -m)=$NITTY" 2>&1)" && fail "packed without readelf" ||
    { grep -q 'readelf' <<<"$out" && ok "without readelf the static check is not skipped: refused" || { fail "readelf message"; echo "$out"; }; }
mkdir -p "$WORK/unpacked" && tar -xzf "$WORK/release/nitty.tar.gz" -C "$WORK/unpacked"
out="$(PATH="$NOGNOME" bash "$WORK/unpacked/nitty-0.0.0-test/install.sh" 2>&1)" && fail "the terminal archive installed as the preview" ||
    { grep -q 'install.sh --terminal' <<<"$out" && ok "its install.sh without --terminal says to add it" || { fail "archive hint"; echo "$out"; }; }
free_port() { python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])'; }
serve() {
    python3 -m http.server "$2" --bind 127.0.0.1 --directory "$1" >/dev/null 2>&1 &
    SERVER=$!
    for _ in $(seq 100); do python3 -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:$2/')" 2>/dev/null && return 0; sleep 0.1; done
    echo "server on port $2 did not start"; exit 1
}
port=$(free_port)
serve "$WORK/release" "$port"
export GNOME_NIFTI_PREVIEW_URL="http://127.0.0.1:$port"
export GNOME_NIFTI_PREVIEW_SERVICE_DIRS=/nonexistent    # no Sushi: not needed
fresh_home
out="$(PATH="$NOGNOME" bash -s -- --terminal < "$ROOT/get.sh" 2>&1)" && [ -x "$HOME/.local/bin/nitty" ] && ok "get.sh --terminal installs without GNOME or Sushi" || { fail "get.sh --terminal"; echo "$out"; }
[ -z "$(find "$HOME/.cache" -maxdepth 1 -name 'gnome-nifti-preview-get.*' 2>/dev/null)" ] && ok "download directory removed" || fail "download directory left"
PATH="$NOGNOME" bash -s -- --terminal --uninstall < "$ROOT/get.sh" >/dev/null 2>&1 && cleaned && ok "get.sh --terminal --uninstall removes it" || fail "get.sh uninstall"
block() { sed -n '/^# >>> remove_nitty/,/^# <<< remove_nitty/p' "$1"; }
[ -n "$(block "$ROOT/get.sh")" ] && [ "$(block "$ROOT/get.sh")" = "$(block "$ROOT/uninstall.sh")" ] && ok "get.sh removes nitty with uninstall.sh's own lines" || fail "the two removals differ"
echo "0000000000000000000000000000000000000000000000000000000000000000  nitty.tar.gz" > "$WORK/release/nitty.tar.gz.sha256"
out="$(PATH="$NOGNOME" bash -s -- --terminal < "$ROOT/get.sh" 2>&1)" && fail "corrupted archive accepted" || { grep -q checksum <<<"$out" && cleaned && ok "corrupted download refused, nothing installed" || { fail "checksum"; echo "$out"; }; }
kill "$SERVER" 2>/dev/null || true; SERVER=""
mkdir -p "$WORK/old"
port=$(free_port)
serve "$WORK/old" "$port"
out="$(GNOME_NIFTI_PREVIEW_URL="http://127.0.0.1:$port" PATH="$NOGNOME" bash -s -- --terminal < "$ROOT/get.sh" 2>&1)" && fail "release without nitty accepted" ||
    { grep -q 'cargo install' <<<"$out" && cleaned && ok "a release without the terminal viewer: says how to build it" || { fail "old release"; echo "$out"; }; }
kill "$SERVER" 2>/dev/null || true; SERVER=""
out="$(bash -s -- --bogus < "$ROOT/get.sh" 2>&1)" && fail "unknown option accepted" || { grep -q 'unknown option' <<<"$out" && ok "unknown get.sh option refused" || { fail "option message"; echo "$out"; }; }

echo "== the two installations ignore each other"
if [ -n "$PREVIEW" ] && command -v gjs >/dev/null && command -v update-mime-database >/dev/null; then
    fresh_home
    "$ROOT/install.sh" --binary "$PREVIEW" --no-restart --quiet 2>/dev/null
    "$ROOT/install.sh" --terminal --binary "$NITTY" --quiet 2>/dev/null
    "$HOME/.local/share/gnome-nifti-preview/uninstall.sh" --quiet >/dev/null 2>&1
    [ -x "$HOME/.local/bin/nitty" ] && [ ! -e "$HOME/.local/share/gnome-nifti-preview" ] && ok "removing the preview keeps nitty" || fail "preview uninstall touched nitty"
    "$ROOT/install.sh" --binary "$PREVIEW" --no-restart --quiet 2>/dev/null
    "$ROOT/uninstall.sh" --terminal --quiet
    [ ! -e "$HOME/.local/bin/nitty" ] && [ -x "$HOME/.local/share/gnome-nifti-preview/bin/nifti-preview" ] && ok "removing nitty keeps the preview" || fail "terminal uninstall touched the preview"
else
    echo "  (skipped: needs NIFTI_PREVIEW, gjs and update-mime-database)"
fi

((failures)) && { echo "$failures failure(s)"; exit 1; }
echo "all terminal installation checks passed"
