#!/usr/bin/env bash
# install.sh / uninstall.sh in a throw-away HOME: expected files, idempotency,
# upgrade cleanup, MIME routing, and an exact rollback on uninstall.
#   tests/install/test_install.sh [BACKEND_BINARY]
set -euo pipefail
unset CDPATH
ROOT="$(cd -- "$(dirname -- "$0")/../.." >/dev/null && pwd)"
BIN="${1:-$ROOT/backend/target/release/nifti-preview}"
[ -x "$BIN" ] || { echo "backend binary not found: $BIN" >&2; exit 2; }
BIN="$(realpath -- "$BIN")"   # some cases run from another folder
. "$ROOT/tests/lib/home.sh"

WORK="$(mktemp -d)"
trap 'rm -rf -- "$WORK"' EXIT
mkdir "$WORK/home" && use_home "$WORK/home"
export XDG_DATA_DIRS="${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
DATA="$HOME/.local/share"
failures=0
ok() { printf '  ok    %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; ci_error "$1"; failures=$((failures + 1)); }
# ~/.cache is written by gio itself (used by the checks), not by the installer.
snapshot() { (cd "$HOME" && find . -mindepth 1 -path ./.cache -prune -o -type f -print0 | sort -z | xargs -0 -r sha256sum; find . -mindepth 1 -path ./.cache -prune -o -type d -print | sort); }

mkdir -p "$HOME/Documents" && echo keep > "$HOME/Documents/notes.txt"
before="$(snapshot)"

echo "== first install"
"$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
for f in gnome-nifti-preview/bin/nifti-preview gnome-nifti-preview/gjs/niftiPreview/backendClient.js gnome-nifti-preview/gjs/niftiPreview/viewState.js \
         sushi/viewers/niftiPreview.js mime/packages/gnome-nifti-preview.xml gnome-nifti-preview/manifest.txt; do
    [ -e "$DATA/$f" ] && ok "$f" || fail "missing $f"
done
[ -x "$DATA/gnome-nifti-preview/bin/nifti-preview" ] && ok "backend is executable" || fail "backend not executable"
"$DATA/gnome-nifti-preview/bin/nifti-preview" --version >/dev/null && ok "installed backend runs" || fail "installed backend does not run"
compgen -G "$DATA/sushi/viewers/.gnome-nifti-preview*" >/dev/null && fail "temporary files left behind" || ok "no temporary files"

python3 - "$HOME/sub 001.nii.gz" <<'EOF'
import gzip, struct, sys
h = bytearray(352); h[0:4] = struct.pack("<i", 348); h[40:48] = struct.pack("<4h", 3, 2, 2, 2)
h[70:74] = struct.pack("<2h", 2, 8); h[76:92] = struct.pack("<4f", 1, 1, 1, 1)
h[108:112] = struct.pack("<f", 352); h[344:348] = b"n+1\0"
open(sys.argv[1], "wb").write(gzip.compress(bytes(h) + bytes(8)))
EOF
t="$(gio info -a standard::content-type "$HOME/sub 001.nii.gz" | sed -n 's/^ *standard::content-type: //p')"
[ "$t" = image/x-compressed-nifti ] && ok "MIME routing active ($t)" || fail "MIME type is $t"
rm "$HOME/sub 001.nii.gz"

echo "== reinstall is idempotent"
first="$(snapshot)"
"$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
second="$(snapshot)"
[ "$first" = "$second" ] && ok "identical tree after a second install" || { fail "tree changed on reinstall"; diff <(echo "$first") <(echo "$second") | head; }

echo "== upgrade removes files no longer shipped"
echo "stale" > "$DATA/gnome-nifti-preview/gjs/niftiPreview/obsolete.js"
echo "$DATA/gnome-nifti-preview/gjs/niftiPreview/obsolete.js" >> "$DATA/gnome-nifti-preview/manifest.txt"
"$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
[ ! -e "$DATA/gnome-nifti-preview/gjs/niftiPreview/obsolete.js" ] && ok "obsolete file removed" || fail "obsolete file kept"

echo "== uninstall restores the previous state"
"$ROOT/uninstall.sh" --no-restart --quiet
after="$(snapshot)"
[ "$before" = "$after" ] && ok "home directory identical to before install" || { fail "leftovers after uninstall"; diff <(echo "$before") <(echo "$after") | head -20; }
"$ROOT/uninstall.sh" --no-restart --quiet && ok "second uninstall is a no-op" || fail "second uninstall failed"

echo "== pre-existing user MIME database is preserved"
mkdir -p "$DATA/mime/packages"
cat > "$DATA/mime/packages/other-app.xml" <<'EOF'
<?xml version="1.0"?>
<mime-info xmlns="http://www.freedesktop.org/standards/shared-mime-info">
  <mime-type type="application/x-other-app"><glob pattern="*.otherapp"/></mime-type>
</mime-info>
EOF
update-mime-database "$DATA/mime"
"$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
"$ROOT/uninstall.sh" --no-restart --quiet
[ -f "$DATA/mime/packages/other-app.xml" ] && ok "other application's MIME package kept" || fail "other MIME package removed"
echo "content" > "$HOME/x.otherapp"  # empty files are always application/x-zerosize
t="$(gio info -a standard::content-type "$HOME/x.otherapp" | sed -n 's/^ *standard::content-type: //p')"
[ "$t" = application/x-other-app ] && ok "other application's MIME type still works" || fail "other MIME type broken ($t)"

echo "== Open With: an installed NIfTI viewer becomes the default"
# Each case runs in a fresh home. The fake ITK-SNAP records how it was called.
fresh() {
    use_home "$(mktemp -d "$WORK/home.XXXXXX")"; DATA="$HOME/.local/share"
}
fake_viewer() { # path
    mkdir -p "$(dirname "$1")"
    printf '#!/bin/sh\necho "$@" > "%s/viewer-args"\n' "$HOME" > "$1"; chmod +x "$1"
}
default_name() { # what Sushi shows: "Open With <name>"
    gjs -c "const {Gio} = imports.gi; try { print(Gio.File.new_for_path(ARGV[0]).query_default_handler(null).get_display_name()); } catch (e) { print('none'); }" "$1" 2>/dev/null
}
nifti() { # path: smallest valid .nii.gz
    python3 - "$1" <<'PY'
import gzip, struct, sys
h = bytearray(352); h[0:4] = struct.pack("<i", 348); h[40:48] = struct.pack("<4h", 3, 2, 2, 2)
h[70:74] = struct.pack("<2h", 2, 8); h[76:92] = struct.pack("<4f", 1, 1, 1, 1)
h[108:112] = struct.pack("<f", 352); h[344:348] = b"n+1\0"
open(sys.argv[1], "wb").write(gzip.compress(bytes(h) + bytes(8)))
PY
}
OUTER_HOME="$HOME"

# A. ITK-SNAP unpacked from the official download (no launcher, not in PATH).
fresh; before="$(snapshot)"
fake_viewer "$HOME/itksnap-4.2.2-20241202-Linux-gcc64/bin/itksnap"
before_a="$(snapshot)"
out="$("$ROOT/install.sh" --binary "$BIN" --no-restart 2>&1)"
grep -q "open with ITK-SNAP" <<<"$out" && ok "installer says NIfTI files open with ITK-SNAP" || fail "no message: $out"
grep -q 'Exec="'"$HOME"'/itksnap-4.2.2-20241202-Linux-gcc64/bin/itksnap" %f' "$DATA/applications/gnome-nifti-preview-itksnap.desktop" 2>/dev/null \
    && ok "hidden launcher created for the unpacked ITK-SNAP" || fail "launcher missing"
nifti "$HOME/sub.nii.gz"; cp "$HOME/sub.nii.gz" "$HOME/plain.nii"; gunzip -c "$HOME/sub.nii.gz" > "$HOME/plain.nii"
[ "$(default_name "$HOME/sub.nii.gz")" = ITK-SNAP ] && ok "Sushi would show \"Open With ITK-SNAP\" (.nii.gz)" || fail "default for .nii.gz is $(default_name "$HOME/sub.nii.gz")"
[ "$(default_name "$HOME/plain.nii")" = ITK-SNAP ] && ok "and for .nii" || fail "default for .nii is $(default_name "$HOME/plain.nii")"
gio open "$HOME/sub.nii.gz" 2>/dev/null; for _ in $(seq 50); do [ -s "$HOME/viewer-args" ] && break; sleep 0.1; done
[ "$(cat "$HOME/viewer-args" 2>/dev/null)" = "$HOME/sub.nii.gz" ] && ok "opening the file launches ITK-SNAP with it" || fail "viewer called with: $(cat "$HOME/viewer-args" 2>/dev/null)"
rm -f "$HOME/sub.nii.gz" "$HOME/plain.nii" "$HOME/viewer-args"
first="$(snapshot)"; "$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
[ "$first" = "$(snapshot)" ] && ok "reinstall leaves the default unchanged" || fail "reinstall changed files"
"$ROOT/uninstall.sh" --no-restart --quiet
[ "$before_a" = "$(snapshot)" ] && ok "uninstall restores the home directory (no ~/.config left)" || { fail "leftovers"; diff <(echo "$before_a") <(snapshot) | head; }

# B. Existing mimeapps.list with other choices: back byte for byte after uninstall.
fresh
fake_viewer "$HOME/opt/ITK-SNAP 4.2/bin/itksnap"
mkdir -p "$HOME/.config"
printf '[Default Applications]\ntext/plain=org.gnome.TextEditor.desktop\n\n[Removed Associations]\nimage/png=foo.desktop;\n' > "$HOME/.config/mimeapps.list"
cp "$HOME/.config/mimeapps.list" "$HOME/mimeapps.orig"
"$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
grep -q '^image/x-nifti=gnome-nifti-preview-itksnap.desktop' "$HOME/.config/mimeapps.list" && ok "default added next to the user's other choices (path with a space)" || fail "default not set"
"$ROOT/uninstall.sh" --no-restart --quiet
cmp -s "$HOME/.config/mimeapps.list" "$HOME/mimeapps.orig" && ok "mimeapps.list identical after uninstall" || { fail "mimeapps.list changed"; diff "$HOME/mimeapps.orig" "$HOME/.config/mimeapps.list"; }

# Edits between installs survive a subsequently discovered viewer.
echo "== external edits survive viewer discovery and repeated installs"
for initial in existing created; do
    fresh
    if [ "$initial" = existing ]; then
        mkdir -p "$HOME/.config"
        printf '# original\n[Default Applications]\ntext/plain=editor.desktop;\n' > "$HOME/.config/mimeapps.list"
    fi
    "$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
    printf '\n[UserSettings]\nkeep=created-between-installs\n' >> "$HOME/.config/mimeapps.list"
    chmod 640 "$HOME/.config/mimeapps.list"
    fake_viewer "$HOME/itksnap-4.2/bin/itksnap"
    "$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
    [ "$(stat -c %a "$HOME/.config/mimeapps.list")" = 640 ] \
        && ok "$initial: discovery preserves the current MIME file mode" || fail "$initial: discovery changed MIME permissions"
    grep -q '^image/x-nifti=gnome-nifti-preview-itksnap.desktop' "$HOME/.config/mimeapps.list" \
        && ok "$initial: viewer discovery changes the association" || fail "$initial: regression scenario did not change the viewer"
    "$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
    grep -qx 'mimeapps_restore_exact=0' "$DATA/gnome-nifti-preview/state" \
        && ok "$initial: external edit remains recorded on reinstall" || fail "$initial: edit history lost"
    "$ROOT/uninstall.sh" --no-restart --quiet
    grep -qx 'keep=created-between-installs' "$HOME/.config/mimeapps.list" \
        && ok "$initial: user edit preserved after uninstall" || fail "$initial: user edit lost"
    [ "$(stat -c %a "$HOME/.config/mimeapps.list")" = 640 ] \
        && ok "$initial: MIME file permissions preserved" || fail "$initial: MIME file permissions changed"
done

# C. The user already chose an application for NIfTI files: never overridden.
fresh
fake_viewer "$HOME/itksnap-4.2/bin/itksnap"
mkdir -p "$DATA/applications" "$HOME/.config"
printf '[Desktop Entry]\nType=Application\nName=My Viewer\nExec=true %%f\nMimeType=image/x-nifti;\n' > "$DATA/applications/my-viewer.desktop"
printf '[Default Applications]\nimage/x-nifti=my-viewer.desktop\n' > "$HOME/.config/mimeapps.list"
cp "$HOME/.config/mimeapps.list" "$HOME/mimeapps.orig"
"$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
cmp -s "$HOME/.config/mimeapps.list" "$HOME/mimeapps.orig" && [ ! -e "$DATA/applications/gnome-nifti-preview-itksnap.desktop" ] \
    && ok "the user's own choice is kept" || fail "user default changed"
"$ROOT/uninstall.sh" --no-restart --quiet
cmp -s "$HOME/.config/mimeapps.list" "$HOME/mimeapps.orig" && ok "and still kept after uninstall" || fail "user default changed by uninstall"

# D. A packaged ITK-SNAP with its own desktop entry is used as is.
fresh
fake_viewer "$HOME/bin/itksnap"
mkdir -p "$DATA/applications"
printf '[Desktop Entry]\nType=Application\nName=ITK-SNAP\nExec=itksnap %%f\n' > "$DATA/applications/itksnap.desktop"
PATH="$HOME/bin:$PATH" "$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
grep -q '^image/x-compressed-nifti=itksnap.desktop' "$HOME/.config/mimeapps.list" 2>/dev/null && [ ! -e "$DATA/applications/gnome-nifti-preview-itksnap.desktop" ] \
    && ok "existing ITK-SNAP desktop entry used, no extra launcher" || fail "desktop entry not used"
PATH="$HOME/bin:$PATH" "$ROOT/uninstall.sh" --no-restart --quiet
[ ! -e "$HOME/.config/mimeapps.list" ] && [ -f "$DATA/applications/itksnap.desktop" ] && ok "uninstall removes only what it added" || fail "uninstall removed too much or too little"

# E. --no-default-app.
fresh
fake_viewer "$HOME/itksnap-4.2/bin/itksnap"
"$ROOT/install.sh" --binary "$BIN" --no-restart --quiet --no-default-app
[ ! -e "$HOME/.config" ] && [ ! -e "$DATA/applications" ] && ok "--no-default-app changes no association" || fail "--no-default-app ignored"
"$ROOT/uninstall.sh" --no-restart --quiet

# F. No NIfTI viewer: the archive tools inherited from application/gzip are not
# offered for .nii.gz ("Open With Files" would extract the file), and come back
# on uninstall; .gz archives keep them.
fresh
mkdir -p "$DATA/applications" "$HOME/.config"
printf '[Desktop Entry]\nType=Application\nName=Archives\nExec=true %%U\nMimeType=application/gzip;\n' > "$DATA/applications/np-test-archives.desktop"
printf '[Desktop Entry]\nType=Application\nName=Both\nExec=true %%U\nMimeType=application/gzip;image/x-compressed-nifti;\n' > "$DATA/applications/np-test-both.desktop"
update-desktop-database "$DATA/applications"   # as when they are installed
printf '[Default Applications]\ntext/plain=org.gnome.TextEditor.desktop\n' > "$HOME/.config/mimeapps.list"
cp "$HOME/.config/mimeapps.list" "$HOME/mimeapps.orig"
before_f="$(snapshot)"
"$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
nifti "$HOME/sub.nii.gz"; printf 'x' | gzip > "$HOME/plain.gz"
apps_for() { gjs -c 'print(imports.gi.Gio.AppInfo.get_all_for_type(ARGV[0]).map(a => a.get_id()).join(","))' "$1" 2>/dev/null; }
nifti_apps="$(apps_for image/x-compressed-nifti)"
[[ ",$nifti_apps," != *,np-test-archives.desktop,* && ",$nifti_apps," == *,np-test-both.desktop,* ]] \
    && ok "archive tool hidden for .nii.gz, an application declaring NIfTI kept ($nifti_apps)" || fail "applications for .nii.gz: $nifti_apps"
[[ ",$(apps_for application/gzip)," == *,np-test-archives.desktop,* ]] && ok "still offered for .gz archives" || fail "gzip apps: $(apps_for application/gzip)"
[ "$(default_name "$HOME/sub.nii.gz")" != Archives ] && ok "Sushi does not show \"Open With Archives\"" || fail "Open With Archives"
grep -q '^text/plain=org.gnome.TextEditor.desktop' "$HOME/.config/mimeapps.list" && ok "the user's other choices kept" || fail "mimeapps.list lost entries"
rm -f "$HOME/sub.nii.gz" "$HOME/plain.gz"
first="$(snapshot)"; "$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
[ "$first" = "$(snapshot)" ] && ok "reinstall changes nothing" || { fail "reinstall changed files"; diff <(echo "$first") <(snapshot) | head; }
"$ROOT/uninstall.sh" --no-restart --quiet
cmp -s "$HOME/.config/mimeapps.list" "$HOME/mimeapps.orig" && [ "$before_f" = "$(snapshot)" ] \
    && ok "uninstall offers the archive tool again (mimeapps.list identical)" || { fail "not restored"; diff "$HOME/mimeapps.orig" "$HOME/.config/mimeapps.list"; }

# G. A GNOME desktop: gnome-mimeapps.list makes Files the default application
# of gzip archives, which .nii.gz inherits. Without a viewer, the default
# becomes our launcher explaining what to install; a viewer installed later
# replaces it; uninstall restores everything.
fresh
export XDG_CURRENT_DESKTOP=GNOME
mkdir -p "$DATA/applications"
printf '[Desktop Entry]\nType=Application\nName=Archives\nExec=true %%U\nMimeType=application/gzip;\n' > "$DATA/applications/np-test-archives.desktop"
printf '[Default Applications]\napplication/gzip=np-test-archives.desktop\n' > "$DATA/applications/gnome-mimeapps.list"
nifti "$HOME/sub.nii.gz"
"$ROOT/install.sh" --binary "$BIN" --no-restart --quiet --no-default-app
[ "$(default_name "$HOME/sub.nii.gz")" = Archives ] && ok "inherited archive default reproduced" || fail "setup: default is $(default_name "$HOME/sub.nii.gz")"
"$ROOT/uninstall.sh" --no-restart --quiet
before_g="$(snapshot)"
out="$("$ROOT/install.sh" --binary "$BIN" --no-restart 2>&1)"
[[ "$(default_name "$HOME/sub.nii.gz")" == "a NIfTI viewer"* ]] && ok "Sushi would show \"Open With a NIfTI viewer…\"" || fail "default is $(default_name "$HOME/sub.nii.gz")"
grep -q "No NIfTI viewer found" <<<"$out" && ok "the installer says no viewer was found" || fail "no message: $out"
grep -q '^Exec=gjs ".*/gnome-nifti-preview/gjs/niftiPreview/noViewer.js" %f$' "$DATA/applications/gnome-nifti-preview-no-viewer.desktop" \
    && ok "launcher runs noViewer.js" || fail "launcher: $(cat "$DATA/applications/gnome-nifti-preview-no-viewer.desktop" 2>&1)"
gjs "$DATA/gnome-nifti-preview/gjs/niftiPreview/noViewer.js" --print "$HOME/sub.nii.gz" | grep -q "ITK-SNAP, FSLeyes, MRIcroGL or MRIcron can open" \
    && ok "it explains what to install" || fail "noViewer.js text"
first="$(snapshot)"; "$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
[ "$first" = "$(snapshot)" ] && ok "reinstall changes nothing" || { fail "reinstall changed files"; diff <(echo "$first") <(snapshot) | head; }
fake_viewer "$HOME/itksnap-4.2/bin/itksnap"
"$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
[ "$(default_name "$HOME/sub.nii.gz")" = ITK-SNAP ] && ok "a viewer installed later becomes the default" || fail "default is $(default_name "$HOME/sub.nii.gz")"
"$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
[ ! -e "$DATA/applications/gnome-nifti-preview-no-viewer.desktop" ] && ok "the launcher is removed once replaced" || fail "launcher left"
"$ROOT/uninstall.sh" --no-restart --quiet
rm -rf "$HOME/itksnap-4.2" "$HOME/viewer-args"
[ "$before_g" = "$(snapshot)" ] && [ "$(default_name "$HOME/sub.nii.gz")" = Archives ] \
    && ok "uninstall restores the home directory and the inherited default" || { fail "not restored"; diff <(echo "$before_g") <(snapshot) | head; }
unset XDG_CURRENT_DESKTOP
use_home "$OUTER_HOME"; DATA="$HOME/.local/share"

echo "== hostile environments"
# root (or sudo) is refused before anything is written.
fresh
mkdir -p "$WORK/fakeid"
printf '#!/bin/sh\n[ "$1" = -u ] && { echo 0; exit 0; }\nexec /usr/bin/id "$@"\n' > "$WORK/fakeid/id"; chmod +x "$WORK/fakeid/id"
before_r="$(snapshot)"
for script in install.sh uninstall.sh; do
    out="$(env -u GNOME_NIFTI_PREVIEW_ALLOW_ROOT PATH="$WORK/fakeid:$PATH" "$ROOT/$script" --no-restart 2>&1)" && fail "$script ran as root" ||
        { grep -q 'do not run this with sudo' <<<"$out" && [ "$before_r" = "$(snapshot)" ] && ok "$script refuses root" || fail "$script as root: $out"; }
done
if [ "$(id -u)" = 0 ]; then
    other="$(mktemp -d "$WORK/other.XXXXXX")"; chown 65534 "$other"
    out="$(HOME="$other" "$ROOT/install.sh" --binary "$BIN" --no-restart 2>&1)" && fail "installed into another user's home" ||
        { grep -q 'belongs to another user' <<<"$out" && [ -z "$(ls -A "$other")" ] && ok "a home folder of another user is refused (sudo -E)" || fail "other home: $out"; }
fi

# A relative XDG_DATA_HOME is ignored (XDG spec), not resolved against the current folder.
fresh; mkdir -p "$WORK/cwd"
(cd "$WORK/cwd" && XDG_DATA_HOME=rel "$ROOT/install.sh" --binary "$BIN" --no-restart --quiet)
[ ! -e "$WORK/cwd/rel" ] && [ -x "$DATA/gnome-nifti-preview/bin/nifti-preview" ] && ok "relative XDG_DATA_HOME ignored" || fail "relative XDG_DATA_HOME used"
(cd "$WORK/cwd" && XDG_DATA_HOME=rel "$ROOT/uninstall.sh" --no-restart --quiet)

# A damaged manifest cannot delete anything but our own files.
fresh
echo precious > "$HOME/precious.txt"
mkdir -p "$DATA/keyrings" && echo secret > "$DATA/keyrings/login.keyring"
"$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
printf '%s\n' "$DATA/gnome-nifti-preview/../../../precious.txt" "$DATA/keyrings/login.keyring" \
    "$DATA/sushi/viewers/../../keyrings/login.keyring" "$HOME/precious.txt" >> "$DATA/gnome-nifti-preview/manifest.txt"
"$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
printf '%s\n' "$DATA/gnome-nifti-preview/../../../precious.txt" "$DATA/keyrings/login.keyring" >> "$DATA/gnome-nifti-preview/manifest.txt"
"$ROOT/uninstall.sh" --no-restart --quiet
[ -f "$HOME/precious.txt" ] && [ -f "$DATA/keyrings/login.keyring" ] && [ ! -e "$DATA/gnome-nifti-preview" ] \
    && ok "manifest entries outside our files are never deleted" || fail "a manifest entry deleted another file"

# mimeapps.list that GLib cannot parse, or a symbolic link: left exactly as it is.
fresh
fake_viewer "$HOME/itksnap-4.2/bin/itksnap"
mkdir -p "$HOME/.config"
printf '[Default Applications]\ntext/plain=org.gnome.TextEditor.desktop\nnot a key file line\n' > "$HOME/.config/mimeapps.list"
cp "$HOME/.config/mimeapps.list" "$HOME/mimeapps.orig"
out="$("$ROOT/install.sh" --binary "$BIN" --no-restart --quiet 2>&1)"
cmp -s "$HOME/.config/mimeapps.list" "$HOME/mimeapps.orig" && grep -q 'not a valid key file' <<<"$out" \
    && ok "an invalid mimeapps.list is not rewritten" || fail "invalid mimeapps.list: $out"
"$ROOT/uninstall.sh" --no-restart --quiet
fresh
fake_viewer "$HOME/itksnap-4.2/bin/itksnap"
mkdir -p "$HOME/dotfiles" "$HOME/.config"
printf '[Default Applications]\ntext/plain=org.gnome.TextEditor.desktop\n' > "$HOME/dotfiles/mimeapps.list"
ln -s ../dotfiles/mimeapps.list "$HOME/.config/mimeapps.list"
out="$("$ROOT/install.sh" --binary "$BIN" --no-restart --quiet 2>&1)"
[ -L "$HOME/.config/mimeapps.list" ] && grep -qx 'text/plain=org.gnome.TextEditor.desktop' "$HOME/dotfiles/mimeapps.list" &&
    ! grep -q nifti "$HOME/dotfiles/mimeapps.list" && grep -q 'symbolic link' <<<"$out" \
    && ok "a symlinked mimeapps.list is left alone" || fail "symlinked mimeapps.list: $out"
"$ROOT/uninstall.sh" --no-restart --quiet
# Linked after the install (dotfiles adopted it): uninstall writes through the link.
fresh
fake_viewer "$HOME/itksnap-4.2/bin/itksnap"
mkdir -p "$HOME/.config" "$HOME/dotfiles"
printf '[Default Applications]\ntext/plain=org.gnome.TextEditor.desktop\n' > "$HOME/.config/mimeapps.list"
cp "$HOME/.config/mimeapps.list" "$HOME/mimeapps.orig"
"$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
mv "$HOME/.config/mimeapps.list" "$HOME/dotfiles/mimeapps.list"; chmod 640 "$HOME/dotfiles/mimeapps.list"
ln -s ../dotfiles/mimeapps.list "$HOME/.config/mimeapps.list"
"$ROOT/uninstall.sh" --no-restart --quiet
[ -L "$HOME/.config/mimeapps.list" ] && cmp -s "$HOME/dotfiles/mimeapps.list" "$HOME/mimeapps.orig" &&
    [ "$(stat -c %a "$HOME/dotfiles/mimeapps.list")" = 640 ] \
    && ok "uninstall keeps the link and the file mode" || fail "uninstall replaced the link or changed the mode"

# A viewer that another user could replace is never made the default: in a
# folder others can write to, in PATH, or behind a desktop entry.
fresh
mkdir -p "$HOME/itksnap-shared"
fake_viewer "$HOME/itksnap-shared/bin/itksnap"
chmod 777 "$HOME/itksnap-shared"
fake_viewer "$HOME/shared-bin/itksnap"; chmod 777 "$HOME/shared-bin/itksnap"
mkdir -p "$DATA/applications" "$HOME/shared-apps/applications"
fake_viewer "$HOME/tools/fsleyes"; chmod 777 "$HOME/tools/fsleyes"
printf '[Desktop Entry]\nType=Application\nName=FSLeyes\nExec=%s %%f\n' "$HOME/tools/fsleyes" > "$DATA/applications/fsleyes.desktop"
fake_viewer "$HOME/tools/mricron"
printf '[Desktop Entry]\nType=Application\nName=MRIcron\nExec=mricron %%f\n' > "$HOME/shared-apps/applications/mricron.desktop"
chmod 666 "$HOME/shared-apps/applications/mricron.desktop"
PATH="$HOME/shared-bin:$HOME/tools:$PATH" XDG_DATA_DIRS="$HOME/shared-apps:$XDG_DATA_DIRS" \
    "$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
# ITK-SNAP and FSLeyes are skipped; MRIcron's entry is skipped but its
# program is safe: it gets a launcher of ours.
! compgen -G "$DATA/applications/gnome-nifti-preview-itksnap*" >/dev/null &&
    grep -qx 'image/x-nifti=gnome-nifti-preview-mricron.desktop' "$HOME/.config/mimeapps.list" &&
    ! grep -qsE 'itksnap|fsleyes|=mricron\.desktop' "$HOME/.config/mimeapps.list" \
    && ok "viewers others can change are ignored (folder, PATH, desktop entry or its program)" || { fail "an untrusted viewer was used"; cat "$HOME/.config/mimeapps.list"; }
"$ROOT/uninstall.sh" --no-restart --quiet

# A viewer folder whose name has shell and desktop-entry characters is used,
# and the launcher passes its path intact.
fresh
odd="$HOME/itksnap \"\$(boom)\" \\ \`x\` it's"
fake_viewer "$odd/bin/itksnap"
"$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
nifti "$HOME/sub.nii.gz"
gio open "$HOME/sub.nii.gz" 2>/dev/null; for _ in $(seq 50); do [ -s "$HOME/viewer-args" ] && break; sleep 0.1; done
[ "$(cat "$HOME/viewer-args" 2>/dev/null)" = "$HOME/sub.nii.gz" ] && ok "oddly named viewer folder: launched with the file" || fail "odd viewer: $(cat "$HOME/viewer-args" 2>/dev/null)"
if command -v desktop-file-validate >/dev/null; then
    desktop-file-validate "$DATA/applications/gnome-nifti-preview-itksnap.desktop" && ok "its launcher is a valid desktop entry" || fail "invalid launcher"
fi
"$ROOT/uninstall.sh" --no-restart --quiet

# A home folder with such characters: the "no viewer" launcher still works
# (instead of letting Files extract the file).
use_home "$(mktemp -d "$WORK/home 100% \"\$x\" \\ q.XXXXXX")"; DATA="$HOME/.local/share"
mkdir -p "$HOME/fakebin"
printf '#!/bin/sh\nprintf "%%s\\n" "$@" > "$HOME/gjs-args"\n' > "$HOME/fakebin/gjs"; chmod +x "$HOME/fakebin/gjs"
"$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
[ -f "$DATA/applications/gnome-nifti-preview-no-viewer.desktop" ] && ok "special home folder: the \"no viewer\" launcher is installed" || fail "no launcher"
nifti "$HOME/sub.nii.gz"
PATH="$HOME/fakebin:$PATH" gio open "$HOME/sub.nii.gz" 2>/dev/null; for _ in $(seq 50); do [ -s "$HOME/gjs-args" ] && break; sleep 0.1; done
[ "$(cat "$HOME/gjs-args" 2>/dev/null)" = "$(printf '%s\n' "$DATA/gnome-nifti-preview/gjs/niftiPreview/noViewer.js" "$HOME/sub.nii.gz")" ] \
    && ok "and it runs noViewer.js with the file" || fail "launcher ran: $(cat "$HOME/gjs-args" 2>/dev/null)"
"$ROOT/uninstall.sh" --no-restart --quiet
[ ! -e "$DATA/gnome-nifti-preview" ] && [ ! -e "$DATA/applications/gnome-nifti-preview-no-viewer.desktop" ] && ok "and uninstall removes it" || fail "special home: leftovers"

# A symbolic link planted in our folder cannot lead the manifest elsewhere.
fresh
echo precious > "$HOME/precious.txt"
"$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
ln -s "$HOME" "$DATA/gnome-nifti-preview/escape"
echo "$DATA/gnome-nifti-preview/escape/precious.txt" >> "$DATA/gnome-nifti-preview/manifest.txt"
"$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
[ -f "$HOME/precious.txt" ] && ok "reinstall: a manifest entry through a link is not deleted" || fail "reinstall deleted a file through a link"
echo "$DATA/gnome-nifti-preview/escape/precious.txt" >> "$DATA/gnome-nifti-preview/manifest.txt"
"$ROOT/uninstall.sh" --no-restart --quiet
[ -f "$HOME/precious.txt" ] && [ ! -e "$DATA/gnome-nifti-preview" ] && ok "uninstall: nor here" || fail "uninstall deleted a file through a link"

# mimeapps.list with comments, an empty group and blank lines: back byte for
# byte; if something else changed it meanwhile, only our entries go.
fresh
fake_viewer "$HOME/itksnap-4.2/bin/itksnap"
mkdir -p "$HOME/.config"
printf '# my associations\n[Default Applications]\ntext/plain=org.gnome.TextEditor.desktop\n\n[Added Associations]\n\n\n' > "$HOME/.config/mimeapps.list"
cp "$HOME/.config/mimeapps.list" "$HOME/mimeapps.orig"
"$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
"$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
"$ROOT/uninstall.sh" --no-restart --quiet
cmp -s "$HOME/.config/mimeapps.list" "$HOME/mimeapps.orig" && ok "comments, empty groups and blank lines restored" || { fail "mimeapps.list not restored"; diff "$HOME/mimeapps.orig" "$HOME/.config/mimeapps.list"; }
"$ROOT/install.sh" --binary "$BIN" --no-restart --quiet
printf '[Desktop Entry]\nType=Application\nName=Reader\nExec=true %%f\nMimeType=application/pdf;\n' > "$DATA/applications/test-reader.desktop"
gio mime application/pdf test-reader.desktop >/dev/null
"$ROOT/uninstall.sh" --no-restart --quiet
grep -qx 'application/pdf=test-reader.desktop' "$HOME/.config/mimeapps.list" && grep -qx 'text/plain=org.gnome.TextEditor.desktop' "$HOME/.config/mimeapps.list" &&
    ! grep -q nifti "$HOME/.config/mimeapps.list" && ok "a later change is kept, our entries removed" || { fail "later change lost"; cat "$HOME/.config/mimeapps.list"; }

# Restarting the previewer stops this session's previewer only.
if command -v dbus-run-session >/dev/null; then
    fresh
    out="$(env -u GNOME_NIFTI_PREVIEW_NO_RESTART dbus-run-session -- bash -c '
        gjs -c "const {Gio, GLib} = imports.gi;
            Gio.bus_own_name(Gio.BusType.SESSION, \"org.gnome.NautilusPreviewer\", 0, null, null, null);
            new GLib.MainLoop(null, false).run();" &
        owner=$!
        bash -c "exec -a \"dbus-monitor org.gnome.NautilusPreviewer\" sleep 30" &
        bystander=$!
        for _ in $(seq 50); do
            gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
                --method org.freedesktop.DBus.NameHasOwner org.gnome.NautilusPreviewer | grep -q true && break
            sleep 0.1
        done
        "$1" --binary "$2" --quiet
        sleep 0.5
        kill -0 $owner 2>/dev/null && echo "owner alive" || echo "owner stopped"
        kill -0 $bystander 2>/dev/null && echo "bystander alive" || echo "bystander killed"
        kill $owner $bystander 2>/dev/null || true' _ "$ROOT/install.sh" "$BIN" 2>&1)"
    grep -q 'owner stopped' <<<"$out" && grep -q 'bystander alive' <<<"$out" \
        && ok "restart stops the previewer that owns the bus name, nothing else" || fail "restart: $out"
    "$ROOT/uninstall.sh" --no-restart --quiet
fi
use_home "$OUTER_HOME"; DATA="$HOME/.local/share"

echo "== Cargo executable artifact location"
fresh
build_src="$WORK/source build"; mkdir -p "$build_src/backend" "$HOME/tools"
cp -R "$ROOT/sushi" "$ROOT/mime" "$ROOT/install.sh" "$ROOT/uninstall.sh" "$build_src/"
: > "$build_src/backend/Cargo.toml"
cat > "$HOME/tools/cargo" <<'CARGO'
#!/usr/bin/env bash
set -eu
if [ "$1" = --version ]; then echo 'cargo 99.0.0'; exit; fi
[[ " $* " == *" --message-format=json-render-diagnostics "* ]]
python3 - "$NP_TEST_ARTIFACT" <<'JSON'
import json, sys
print(json.dumps({"reason":"compiler-artifact", "target":{"name":"nifti-preview"}, "executable":sys.argv[1]}))
JSON
CARGO
chmod +x "$HOME/tools/cargo"
mkdir -p "$WORK/target elsewhere/release"
cp "$BIN" "$WORK/target elsewhere/release/nifti-preview"
export NP_TEST_ARTIFACT="$WORK/target elsewhere/release/nifti-preview"
PATH="$HOME/tools:$PATH" "$build_src/install.sh" --quiet --no-default-app --no-restart
cmp -s "$NP_TEST_ARTIFACT" "$DATA/gnome-nifti-preview/bin/nifti-preview" \
    && ok "installs Cargo's reported executable outside the source tree" || fail "Cargo artifact not installed"
"$ROOT/uninstall.sh" --no-restart --quiet
unset NP_TEST_ARTIFACT
use_home "$OUTER_HOME"; DATA="$HOME/.local/share"

echo "== missing backend gives a clear error"
if out="$(PATH=/usr/bin:/bin "$ROOT/install.sh" --binary /nonexistent --no-restart 2>&1)"; then fail "install succeeded without binary"
else echo "$out" | grep -q "not executable" && ok "clear error for a bad --binary" || fail "unclear error: $out"; fi

if ((failures)); then echo "$failures install check(s) failed"; exit 1; fi
echo "all install checks passed"
