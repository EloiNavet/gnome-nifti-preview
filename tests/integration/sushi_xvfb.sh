#!/usr/bin/env bash
# End-to-end test with the real, distribution-packaged GNOME Sushi under Xvfb.
#
#   tests/integration/sushi_xvfb.sh FIXTURE_DIR OUT_DIR [BACKEND_BINARY]
#
# Installs GNOME NIfTI Preview into a throw-away HOME, asks Sushi over D-Bus to preview
# files exactly like Nautilus does (org.gnome.NautilusPreviewer2.ShowFile),
# takes screenshots, drives the preview with the mouse and keyboard (xdotool),
# closes it with Space and checks that the backend process is gone. Ordinary
# previews (PNG, text, PDF-less fallback) are checked as well.
#
# What this does NOT cover: Nautilus itself, Wayland, GNOME Shell focus rules.
set -euo pipefail
unset CDPATH
ROOT="$(cd -- "$(dirname -- "$0")/../.." >/dev/null && pwd)"
FIX="$(cd "${1:?fixture dir}" && pwd)"
OUT="$(mkdir -p "${2:?output dir}" && cd "$2" && pwd)"
BIN="${3:-$ROOT/backend/target/release/nifti-preview}"

. "$ROOT/tests/lib/home.sh"
XVFB=""
SANDBOX="$(mktemp -d)" || exit 1
trap '[ -z "$XVFB" ] || kill "$XVFB" 2>/dev/null; rm -rf -- "$SANDBOX"' EXIT
use_home "$SANDBOX"
export XDG_RUNTIME_DIR="$HOME/run"; mkdir -m 700 "$XDG_RUNTIME_DIR"
export XDG_DATA_DIRS="${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
export GSK_RENDERER=cairo LIBGL_ALWAYS_SOFTWARE=1 NO_AT_BRIDGE=1 GTK_A11Y=none
export NIFTI_PREVIEW_DEBUG=1
# A GNOME desktop: gnome-mimeapps.list makes Files the default application of
# gzip archives, which .nii.gz files inherit (see install.sh).
export XDG_CURRENT_DESKTOP=GNOME
mkdir -p "$XDG_DATA_HOME/applications"
printf '[Desktop Entry]\nType=Application\nName=Archives\nExec=true %%U\nMimeType=application/gzip;\n' \
    > "$XDG_DATA_HOME/applications/np-test-archives.desktop"
printf '[Default Applications]\napplication/gzip=np-test-archives.desktop\n' > "$XDG_DATA_HOME/applications/gnome-mimeapps.list"
"$ROOT/install.sh" --binary "$BIN" --no-restart --quiet

# A display number of its own (never an existing X server's).
Xvfb -displayfd 3 -screen 0 1920x1200x24 -nolisten tcp 3>"$HOME/display" >/dev/null 2>&1 &
XVFB=$!
for _ in $(seq 100); do [ -s "$HOME/display" ] && break; sleep 0.05; done
[ -s "$HOME/display" ] || { echo "Xvfb did not start"; exit 1; }
display="$(head -n1 "$HOME/display")"
export DISPLAY=":$display"
for _ in $(seq 50); do xdpyinfo >/dev/null 2>&1 && break; sleep 0.1; done

dbus-run-session -- bash -s "$FIX" "$OUT" <<'SESSION'
set -Eeuo pipefail
# A command that stops this script is reported with its line (GitHub Actions).
[ -z "${GITHUB_ACTIONS:-}" ] || trap 'printf "::error::sushi session line %s: %s\n" "$LINENO" "$BASH_COMMAND"' ERR
FIX="$1"; OUT="$2"
LOG="$OUT/sushi.log"; : > "$LOG"
failures=0
ok() { printf '  ok    %s\n' "$1"; }
# Under GitHub Actions, also an annotation (readable without the raw log).
fail() { printf '  FAIL  %s\n' "$1"; [ -z "${GITHUB_ACTIONS:-}" ] || printf '::error::%s\n' "$1"; failures=$((failures + 1)); }

# Start the previewer ourselves (so that its stderr is captured) and wait for
# its bus name without triggering D-Bus activation of a second instance.
has_owner() {
    gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
        --method org.freedesktop.DBus.NameHasOwner org.gnome.NautilusPreviewer 2>/dev/null | grep -q true
}
# A loaded runner can take a while to start the first GJS process: wait up to
# 30 s, and start it once more if it gave up.
for attempt in 1 2; do
    /usr/libexec/org.gnome.NautilusPreviewer >>"$LOG" 2>&1 &
    for _ in $(seq 300); do has_owner && break; sleep 0.1; done
    has_owner && break
    echo "  Sushi not on the bus after 30 s (attempt $attempt)"
    kill $! 2>/dev/null || true
done
has_owner || { fail "Sushi did not start"; cat "$LOG"; exit 1; }
iface="$(gdbus introspect --session --dest org.gnome.NautilusPreviewer --object-path /org/gnome/NautilusPreviewer)"
nargs=3; grep -q activationToken <<<"$iface" && nargs=4
version="$(dpkg-query -W -f='${Version}' gnome-sushi 2>/dev/null || echo unknown)"
echo "Sushi $version (ShowFile with $nargs arguments)"

show() { # path
    local uri
    uri="$(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).absolute().as_uri())' "$1")"
    if [ "$nargs" = 4 ]; then
        gdbus call --session --dest org.gnome.NautilusPreviewer --object-path /org/gnome/NautilusPreviewer \
            --method org.gnome.NautilusPreviewer2.ShowFile "$uri" "" false "" >/dev/null
    else
        gdbus call --session --dest org.gnome.NautilusPreviewer --object-path /org/gnome/NautilusPreviewer \
            --method org.gnome.NautilusPreviewer2.ShowFile "$uri" "" false >/dev/null
    fi
}
window_for() { # title substring -> window id
    local id
    for _ in $(seq 100); do
        id="$(xdotool search --onlyvisible --name "$1" 2>/dev/null | head -n1 || true)"
        [ -n "$id" ] && { echo "$id"; return 0; }
        sleep 0.05
    done
    return 1
}
geometry() { # window id: sets X Y WIDTH HEIGHT; fails when the window is gone
    local out
    out="$(xdotool getwindowgeometry --shell "$1" 2>/dev/null)" || return 1
    eval "$out"
    [ -n "${WIDTH:-}" ] && [ -n "${HEIGHT:-}" ]
}
shot() { # window id, name: crop the root window (robust while the window animates)
    local X Y WIDTH HEIGHT SCREEN WINDOW
    if ! geometry "$1"; then fail "window gone before screenshot $2"; return 0; fi
    import -window root -crop "${WIDTH}x${HEIGHT}+${X}+${Y}" +repage "$OUT/$2.png" || fail "screenshot $2"
}
shot_changed() { # window id, name, reference: screenshot until it differs (3 s at most)
    for _ in $(seq 15); do
        shot "$1" "$2"
        cmp -s "$OUT/$2.png" "$3" || return 0
        sleep 0.2
    done
    return 1
}
backends() { pgrep -u "$(id -u)" -fc "$HOME/.local/share/gnome-nifti-preview/bin/nifti-preview serve" 2>/dev/null || true; }
sushi_pid() { pgrep -u "$(id -u)" -f '^[^ ]*gjs[^ ]*( -m)? /usr/libexec/org.gnome.NautilusPreviewer' | head -n1; }
wait_backends() { # expected count
    for _ in $(seq 60); do [ "$(backends)" = "$1" ] && return 0; sleep 0.05; done
    return 1
}
close_with_space() { # window id
    local X Y WIDTH HEIGHT SCREEN WINDOW
    geometry "$1" || return 0   # already gone
    xdotool mousemove $((X + WIDTH / 2)) $((Y + 10))
    xdotool key space
    for _ in $(seq 60); do xdotool search --onlyvisible --name "$2" >/dev/null 2>&1 || return 0; sleep 0.05; done
    return 1
}

python3 - "$FIX" <<'EOF'
import sys, zlib, struct
d = sys.argv[1]
def png(w, h):
    rows = b"".join(b"\0" + b"".join(bytes(((x * 4) % 256, (y * 4) % 256, 160)) for x in range(w)) for y in range(h))
    chunk = lambda t, p: struct.pack(">I", len(p)) + t + p + struct.pack(">I", zlib.crc32(t + p) & 0xffffffff)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b"")
open(f"{d}/gradient.png", "wb").write(png(64, 64))
open(f"{d}/readme.txt", "w").write("Plain text preview still works.\n" * 5)
# A DICOM structured report: a file recognised, without an image.
def el(g, e, vr, val):
    if len(val) % 2:
        val += b"\0" if vr == b"UI" else b" "
    if vr in (b"OB", b"OW", b"SQ", b"UN", b"UT"):
        return struct.pack("<HH", g, e) + vr + b"\0\0" + struct.pack("<I", len(val)) + val
    return struct.pack("<HH", g, e) + vr + struct.pack("<H", len(val)) + val
item = lambda body: struct.pack("<HHI", 0xFFFE, 0xE000, len(body)) + body
meta = el(2, 0x10, b"UI", b"1.2.840.10008.1.2.1")
name = el(8, 0x100, b"SH", b"1") + el(8, 0x104, b"LO", b"Finding")
content = el(0x40, 0xA040, b"CS", b"TEXT") + el(0x40, 0xA043, b"SQ", item(name)) + el(0x40, 0xA160, b"UT", b"No mass.")
report = el(8, 0x16, b"UI", b"1.2.840.10008.5.1.4.1.1.88.11") + el(8, 0x60, b"CS", b"SR") + el(0x40, 0xA730, b"SQ", item(content))
open(f"{d}/report.dcm", "wb").write(b"\0" * 128 + b"DICM" + el(2, 0, b"UL", struct.pack("<I", len(meta))) + meta + report)
EOF

# Warm-up: under Xvfb (no window manager) the very first window of a Sushi 51
# process is undersized whatever the file type; open and close one text preview.
show "$FIX/readme.txt"
if w="$(window_for 'readme.txt')"; then
    gdbus call --session --dest org.gnome.NautilusPreviewer --object-path /org/gnome/NautilusPreviewer \
        --method org.gnome.NautilusPreviewer2.Close >/dev/null || true
    for _ in $(seq 40); do xdotool search --onlyvisible --name readme.txt >/dev/null 2>&1 || break; sleep 0.05; done
fi
: > "$LOG.mark"

# Sushi's title-bar button opens the file with its default application: never
# an archive tool inherited from application/gzip (Files would extract it).
handler="$(gjs -c 'const {Gio} = imports.gi; try { print(Gio.File.new_for_path(ARGV[0]).query_default_handler(null).get_id()); } catch (e) { print("none"); }' "$FIX/sub-001_T1w.nii.gz" 2>/dev/null || true)"
[ "$handler" = gnome-nifti-preview-no-viewer.desktop ] \
    && ok "no archive tool offered for .nii.gz: \"Open With a NIfTI viewer…\"" || fail "the preview would offer $handler"

echo "== NIfTI preview"
t0=$(date +%s%N)
show "$FIX/sub-001_T1w.nii.gz"
win="$(window_for 'sub-001_T1w.nii.gz')" && ok "preview window opened" || { fail "no preview window"; cat "$LOG"; exit 1; }
t1=$(date +%s%N)
for _ in $(seq 250); do grep -q 'three slices shown' "$LOG" && break; sleep 0.02; done
t2=$(date +%s%N)
grep -q 'three slices shown' "$LOG" && ok "three slices displayed" || fail "slices not displayed"
# The side panel appears once, complete (not first without the statistics),
# unless the statistics really took longer than the viewer waits for them.
for _ in $(seq 100); do grep -q 'statistics received' "$LOG" && break; sleep 0.05; done
ms_of() { sed -n "s/.*nifti-preview: $1 after \([0-9.]*\) ms.*/\1/p" "$LOG" | head -n1; }
if grep -q 'metadata shown with statistics' "$LOG"; then
    ok "metadata panel shown once, complete"
elif [ -n "$(ms_of 'metadata shown (statistics pending)')" ] && [ -n "$(ms_of 'statistics received')" ] &&
    awk -v p="$(ms_of 'metadata shown (statistics pending)')" -v s="$(ms_of 'statistics received')" 'BEGIN { exit !(s > p) }'; then
    echo "  note  statistics after $(ms_of 'statistics received') ms (slow machine): the panel waited, then showed placeholders"
else
    fail "metadata panel shown before the statistics"
fi
echo "  window after $(( (t1 - t0) / 1000000 )) ms, three slices after $(( (t2 - t0) / 1000000 )) ms (D-Bus call to paint, Sushi already running)"
grep 'nifti-preview:' "$LOG" | sed 's/^/  log: /' || true
sleep 0.3
shot "$win" "01-t1-neurological"
[ "$(backends)" = 1 ] && ok "one backend process running" || fail "backend count $(backends)"

geometry "$win" || fail "preview window gone"
# 2x2 grid: coronal top-left, sagittal top-right, axial bottom-left
ax=$((X + WIDTH / 4)); ay=$((Y + HEIGHT * 3 / 4))
xdotool mousemove "$ax" "$ay"
for _ in $(seq 12); do xdotool click 4; done   # scroll up = towards superior
sleep 0.3
xdotool mousemove $((X + WIDTH / 4 + 25)) $((Y + HEIGHT / 4 - 20)) click 1   # click in coronal view
sleep 0.4
shot "$win" "02-t1-scrolled-clicked"
cmp -s "$OUT/01-t1-neurological.png" "$OUT/02-t1-scrolled-clicked.png" && fail "scroll/click changed nothing" || ok "scroll and click update the views"
for i in 1 2 3 4 5; do xdotool mousemove $((X + WIDTH / 3 + 8 * i)) $((Y + HEIGHT - 90 + 4 * i)); sleep 0.04; done
sleep 0.35
shot "$win" "04-toolbar"
xdotool mousemove "$ax" "$ay"
xdotool key o
shot_changed "$win" "03-t1-radiological" "$OUT/02-t1-scrolled-clicked.png" && ok "orientation toggle (radiological)" || fail "orientation toggle changed nothing"
xdotool key o; sleep 0.4; xdotool key o; sleep 0.4    # stored, back to neurological
# Keyboard only: V chooses the view Page Up/Down move (axial, under the pointer, then coronal).
xdotool key v
for _ in $(seq 40); do grep -q 'active view coronal' "$LOG" && break; sleep 0.05; done
grep -q 'active view coronal' "$LOG" && ok "V makes the next view the active one" || fail "V did nothing"
shot "$win" "05-keyboard-view"
xdotool key Next
shot_changed "$win" "06-keyboard-page-down" "$OUT/05-keyboard-view.png" && ok "Page Down then moves that view" || fail "Page Down changed nothing"
close_with_space "$win" 'sub-001_T1w.nii.gz' && ok "Space closes the preview" || fail "Space did not close the preview"
wait_backends 0 && ok "backend exited after close" || fail "backend still running ($(backends))"

echo "== showcase (synthetic brain phantom)"
for f in sub-01_T1w.nii.gz sub-01_dseg.nii.gz; do
    [ -f "$FIX/$f" ] || continue
    show "$FIX/$f"
    if win="$(window_for "$f")"; then
        sleep 1.0
        shot "$win" "showcase-${f%%.nii.gz}"
        ok "$f showcase screenshot"
        close_with_space "$win" "$f" || fail "$f did not close"
    else
        fail "$f: no window"
    fi
done

echo "== segmentation, 4D, file name with spaces, LPS storage"
for f in "sub-001_dseg.nii.gz" "sub-001_bold.nii.gz" "sub 001 with spaces.nii" "sub-001_T1w_LPS.nii.gz"; do
    show "$FIX/$f"
    if win="$(window_for "$f")"; then
        sleep 0.8
        shot "$win" "10-$(echo "$f" | tr ' ' '_' | sed 's/\.nii.*//')"
        ok "$f previewed"
        close_with_space "$win" "$f" || fail "$f did not close"
    else
        fail "$f: no window"
    fi
done
wait_backends 0 && ok "no backend left" || fail "backends left: $(backends)"

echo "== oblique acquisition (resampled), RGB colours, the O and L keys"
saturation() { convert "$1" -colorspace HSL -format '%[fx:mean.g]' info:; }
for f in "sub-001_T1w_oblique.nii.gz" "sub-001_rgb.nii.gz"; do
    [ -f "$FIX/$f" ] || continue
    show "$FIX/$f"
    if win="$(window_for "$f")"; then
        sleep 0.8
        name="11-${f%%.nii.gz}"
        shot "$win" "$name"
        ok "$f previewed"
        geometry "$win" || { fail "$f: window gone"; continue; }
        xdotool mousemove $((X + WIDTH / 4)) $((Y + HEIGHT * 3 / 4))
        if [ "$f" = sub-001_rgb.nii.gz ]; then
            xdotool key l
            colour="$(saturation "$OUT/$name.png")"
            for _ in $(seq 15); do
                sleep 0.2; shot "$win" "$name-gray"; gray="$(saturation "$OUT/$name-gray.png")"
                awk -v c="$colour" -v g="$gray" 'BEGIN { exit !(c > g + 0.02) }' && break
            done
            awk -v c="$colour" -v g="$gray" 'BEGIN { exit !(c > g + 0.02) }' \
                && ok "RGB image in colour, L shows it in grayscale (saturation $colour -> $gray)" || fail "RGB colours: saturation $colour, after L $gray"
        else
            xdotool key o; sleep 0.4; xdotool key o   # radiological, stored
            shot_changed "$win" "$name-stored" "$OUT/$name.png" && ok "stored mode shows the voxels as acquired" || fail "stored mode looks the same"
            xdotool key o; sleep 0.4
        fi
        close_with_space "$win" "$f" || fail "$f did not close"
    else
        fail "$f: no window"
    fi
done
wait_backends 0 && ok "no backend left" || fail "backends left: $(backends)"

echo "== spectroscopy (NIfTI-MRS): single voxel and imaging, the L and Z keys"
for f in "sub-001_svs.nii.gz" "sub-001_mrsi.nii.gz"; do
    [ -f "$FIX/$f" ] || continue
    shown="$(grep -c 'spectrum shown' "$LOG" || true)"
    show "$FIX/$f"
    if win="$(window_for "$f")"; then
        for _ in $(seq 100); do [ "$(grep -c 'spectrum shown' "$LOG" || true)" -gt "$shown" ] && break; sleep 0.05; done
        [ "$(grep -c 'spectrum shown' "$LOG" || true)" -gt "$shown" ] && ok "$f: spectrum shown" || fail "$f: no spectrum"
        sleep 0.5
        name="12-${f%%.nii.gz}"
        shot "$win" "$name"
        geometry "$win" || { fail "$f: window gone"; continue; }
        xdotool mousemove $((X + WIDTH * 3 / 4)) $((Y + HEIGHT - 40))   # over the panel, not the plot
        xdotool key l
        shot_changed "$win" "$name-l" "$OUT/$name.png" && ok "$f: L switches real part and magnitude" || fail "$f: L changed nothing"
        if [ "$f" = sub-001_svs.nii.gz ]; then
            xdotool key z
            shot_changed "$win" "$name-z" "$OUT/$name-l.png" && ok "$f: Z shows the whole width" || fail "$f: Z changed nothing"
        fi
        close_with_space "$win" "$f" || fail "$f did not close"
    else
        fail "$f: no window"
    fi
done
wait_backends 0 && ok "no backend left" || fail "backends left: $(backends)"

echo "== invalid file shows Sushi's error page"
show "$FIX/not-a-nifti.nii.gz"
if win="$(window_for 'not-a-nifti')"; then
    sleep 0.6; shot "$win" "20-invalid-file"; ok "error page shown"; close_with_space "$win" 'not-a-nifti' || true
else fail "no window for invalid file"; fi
show "$FIX/truncated.nii.gz"
if win="$(window_for 'truncated')"; then
    sleep 0.8; shot "$win" "21-truncated-file"; ok "truncated file handled"; close_with_space "$win" 'truncated' || true
else fail "no window for truncated file"; fi
wait_backends 0 && ok "no backend left" || fail "backends left: $(backends)"
# A file recognised without an image shows what it holds, not Sushi's error
# page: the viewer (and its backend) stay until the window closes.
show "$FIX/report.dcm"
if win="$(window_for 'report')"; then
    sleep 1.5; shot "$win" "22-report"
    if [ "$(backends)" = 1 ]; then ok "a DICOM report shows its content"; else fail "a DICOM report shows Sushi's error page"; fi
    close_with_space "$win" 'report' || true
else fail "no window for a DICOM report"; fi
wait_backends 0 && ok "no backend left" || fail "backends left: $(backends)"

echo "== ordinary previews are unaffected"
for f in gradient.png readme.txt; do
    show "$FIX/$f"
    if win="$(window_for "$f")"; then sleep 0.6; shot "$win" "30-${f%.*}"; ok "$f previewed by Sushi"; close_with_space "$win" "$f" || fail "$f did not close"
    else fail "$f: no window"; fi
done
[ "$(backends)" = 0 ] && ok "no backend started for ordinary files" || fail "backend started for ordinary files"

echo "== 20 open/close cycles"
pid="$(sushi_pid || true)"
rss0="$(awk '/VmRSS/{print $2}' "/proc/$pid/status" 2>/dev/null || echo 0)"
for i in $(seq 20); do
    show "$FIX/sub-001_dseg.nii.gz"
    w="$(window_for 'sub-001_dseg')" || { fail "cycle $i: no window"; break; }
    sleep 0.25
    close_with_space "$w" 'sub-001_dseg' || { fail "cycle $i: did not close"; break; }
done
wait_backends 0 && ok "no backend left after 20 cycles" || fail "backends left: $(backends)"
rss1="$(awk '/VmRSS/{print $2}' "/proc/$pid/status" 2>/dev/null || echo 0)"
echo "  Sushi RSS: $((rss0 / 1024)) MiB before, $((rss1 / 1024)) MiB after 20 cycles"
zombies="$(ps -eo stat,cmd | awk '$1 ~ /^Z/ && /nifti-preview/' | wc -l)"
[ "$zombies" = 0 ] && ok "no zombie backend" || fail "$zombies zombie backend(s)"

echo "== rapid file changes (arrow keys held down in Files)"
# Each ShowFile replaces the previous preview, often before it has started.
calls=()
for i in $(seq 15); do
    for f in sub-001_T1w.nii.gz sub-001_dseg.nii.gz sub-001_bold.nii.gz; do show "$FIX/$f" & calls+=($!); done
done
wait "${calls[@]}" || true
w="$(window_for 'sub-001_')" && sleep 1
# A replaced preview's backend may still be closing (killed after 1.5 s at most).
for _ in $(seq 40); do [ "$(backends)" -le 1 ] && break; sleep 0.1; done
[ "$(backends)" -le 1 ] && ok "at most one backend while browsing ($(backends))" || fail "$(backends) backends left running"
gdbus call --session --dest org.gnome.NautilusPreviewer --object-path /org/gnome/NautilusPreviewer \
    --method org.gnome.NautilusPreviewer2.Close >/dev/null || true
wait_backends 0 && ok "no backend left after closing" || fail "backends left: $(backends)"

if grep -E 'JS ERROR|Gjs-CRITICAL' "$LOG" | grep -v 'Gjs-CRITICAL.*libEGL' >/dev/null; then
    fail "JavaScript errors in the Sushi log"; grep -E -A3 'JS ERROR|Gjs-CRITICAL' "$LOG" | head -30
else ok "no JavaScript errors in the Sushi log"; fi
if ((failures)); then echo "$failures check(s) failed"; exit 1; fi
echo "all Sushi integration checks passed"
SESSION
