#!/usr/bin/env bash
# GNOME NIfTI Preview — per-user installer (no root needed, nothing outside your home).
#
#   ./install.sh [--binary PATH] [--no-default-app] [--no-restart] [--quiet]
#
# Installs, under ${XDG_DATA_HOME:-~/.local/share}:
#   gnome-nifti-preview/bin/nifti-preview           native backend (built with cargo, or taken from --binary / bin/)
#   gnome-nifti-preview/gjs/niftiPreview/*.js       shared GJS modules
#   sushi/viewers/niftiPreview.js         viewer for Sushi 46–50 (GTK 3)
#   sushi/plugins-1/niftiPreview.js       viewer for Sushi 51+ (GTK 4, experimental)
#   mime/packages/gnome-nifti-preview.xml NIfTI MIME types (.nii, .nii.gz)
#   gnome-nifti-preview/uninstall.sh                removes everything again
# If ITK-SNAP (or FSLeyes, MRIcroGL, MRIcron) is installed and no application
# was chosen for NIfTI files, it becomes their default ("Open With ITK-SNAP" in
# the preview), unless --no-default-app. Running it again upgrades in place.
set -euo pipefail
unset CDPATH

die() { printf 'error: %s\n' "$*" >&2; exit 1; }
# Everything goes into your own home folder: as root (sudo), files would end
# up owned by root there, or in root's home, and the previewers of every user
# would be restarted. Test containers set GNOME_NIFTI_PREVIEW_ALLOW_ROOT=1.
if [ "$(id -u)" -eq 0 ] && [ "${GNOME_NIFTI_PREVIEW_ALLOW_ROOT:-}" != 1 ]; then
    die "do not run this with sudo or as root: it installs into your home folder. Run it again as yourself."
fi
case "${HOME:-}" in /*) ;; *) die "HOME is not set to an absolute path" ;; esac
[ -O "$HOME" ] || die "your home folder $HOME belongs to another user (sudo -E?); run this as yourself."

SRC="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null && pwd)"
# XDG paths must be absolute; relative ones are ignored (XDG base directory spec).
DATA="${XDG_DATA_HOME:-}"
case "$DATA" in /*) ;; *) DATA="$HOME/.local/share" ;; esac
CONFIG="${XDG_CONFIG_HOME:-}"
case "$CONFIG" in /*) ;; *) CONFIG="$HOME/.config" ;; esac
APP="$DATA/gnome-nifti-preview"
MANIFEST="$APP/manifest.txt"
MIN_RUST="1.85"
BINARY=""
RESTART=1
[ "${GNOME_NIFTI_PREVIEW_NO_RESTART:-}" != 1 ] || RESTART=0   # tests
QUIET=0
DEFAULT_APP=1

usage() { sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; }
while (($#)); do
    case "$1" in
        --binary) BINARY="${2:?--binary needs a path}"; shift 2 ;;
        --no-restart) RESTART=0; shift ;;
        --no-default-app) DEFAULT_APP=0; shift ;;
        --quiet) QUIET=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 64 ;;
    esac
done

say() { ((QUIET)) || printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }

# Temporary files, all removed on exit whatever happens.
TMPS=()
cleanup() { if ((${#TMPS[@]})); then rm -f -- "${TMPS[@]}"; fi; }
trap cleanup EXIT
new_tmp() { # directory -> $REPLY, a new temporary file there
    REPLY="$(mktemp "$1/.gnome-nifti-preview.XXXXXX")"
    TMPS+=("$REPLY")
}

version_ge() { [ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -n1)" = "$2" ]; }

# ------------------------------------------------------------ requirements
missing=()
command -v gjs >/dev/null || missing+=(gjs)
command -v update-mime-database >/dev/null || missing+=(shared-mime-info)
sushi_version=""
if command -v dpkg-query >/dev/null; then
    # Only an installed package counts ("ii"; "rc" means removed, config left).
    status="$(dpkg-query -W -f='${db:Status-Abbrev}|${Version}' gnome-sushi 2>/dev/null || true)"
    case "$status" in ii*) sushi_version="${status#*|}" ;; esac
fi
metainfo_version() { # file -> first release version, if any
    [ -f "$1" ] || return 0
    sed -n 's/.*<release[^>]* version="\([^"]*\)".*/\1/p' "$1" | head -n1 || true
}
for f in /usr/share/metainfo/org.gnome.NautilusPreviewer.metainfo.xml /usr/share/metainfo/org.gnome.NautilusPreviewer.appdata.xml; do
    [ -n "$sushi_version" ] || sushi_version="$(metainfo_version "$f")"
done
[ -n "$sushi_version" ] || missing+=(gnome-sushi)
if ((${#missing[@]})); then
    warn "missing system packages: ${missing[*]}"
    warn "install them with:  sudo apt install ${missing[*]}"
    [[ " ${missing[*]} " == *" gjs "* || " ${missing[*]} " == *" shared-mime-info "* ]] && die "cannot continue without gjs and shared-mime-info"
fi
if [ -n "$sushi_version" ]; then
    major="${sushi_version%%.*}"; major="${major##*:}"
    case "$major" in
        4[6-9]|50) say "GNOME Sushi $sushi_version detected (GTK 3 viewer)." ;;
        5[1-9]|[6-9][0-9]) say "GNOME Sushi $sushi_version detected (GTK 4 viewer, experimental)." ;;
        *) warn "GNOME Sushi $sushi_version is older than 46 and has not been tested." ;;
    esac
fi

# ------------------------------------------------------------ backend binary
arch="$(uname -m)"
find_cargo() {
    local c best="" bestv=""
    for c in cargo $(compgen -c cargo- 2>/dev/null | grep -E '^cargo-1\.[0-9]+$' | sort -uV); do
        command -v "$c" >/dev/null || continue
        local v
        v="$("$c" --version 2>/dev/null | awk '{print $2}')" || continue
        [ -n "$v" ] || continue
        if version_ge "$v" "$MIN_RUST" && { [ -z "$bestv" ] || version_ge "$v" "$bestv"; }; then
            best="$c"; bestv="$v"
        fi
    done
    [ -n "$best" ] && echo "$best"
}

if [ -z "$BINARY" ] && [ -x "$SRC/bin/$arch/nifti-preview" ]; then
    BINARY="$SRC/bin/$arch/nifti-preview"        # release tarball layout
fi
if [ -z "$BINARY" ] && [ -f "$SRC/backend/Cargo.toml" ]; then
    if cargo_cmd="$(find_cargo)"; then
        say "Building the backend with $cargo_cmd ($("$cargo_cmd" --version | awk '{print $2}'))..."
        # Ubuntu's versioned toolchains (cargo-1.85, ...) need the matching rustc.
        rustc_cmd="$(command -v "${cargo_cmd/cargo/rustc}" || command -v rustc || true)"
        quiet_flag=""; ((QUIET)) && quiet_flag="--quiet"
        new_tmp "${TMPDIR:-/tmp}"
        build_messages="$REPLY"
        (cd "$SRC/backend" && RUSTC="${RUSTC:-$rustc_cmd}" "$cargo_cmd" build --release --locked \
            --message-format=json-render-diagnostics $quiet_flag > "$build_messages") \
            || die "the backend build failed (see the messages above)"
        BINARY="$(gjs -c '
            const text = imports.byteArray.toString(imports.gi.GLib.file_get_contents(ARGV[0])[1]);
            for (const line of text.split("\n")) {
                if (!line) continue;
                const m = JSON.parse(line);
                if (m.reason === "compiler-artifact" && m.target.name === "nifti-preview" && m.executable)
                    print(m.executable);
            }
        ' "$build_messages")"
    elif [ -x "$SRC/backend/target/release/nifti-preview" ]; then
        BINARY="$SRC/backend/target/release/nifti-preview"
        warn "cargo not found; using the previously built $BINARY"
    fi
fi
if [ -z "$BINARY" ]; then
    die "no backend binary. Either install Rust >= $MIN_RUST and run this script again
       (Ubuntu 26.04: sudo apt install cargo build-essential
        Ubuntu 24.04: sudo apt install cargo-1.85 build-essential),
       or pass a prebuilt binary with --binary PATH."
fi
[ -x "$BINARY" ] || die "$BINARY is not executable"
backend_version="$("$BINARY" --version 2>/dev/null)" || die "$BINARY does not run on this system"
say "Backend: $backend_version"

# ------------------------------------------------------------ install files
# Remember whether the user MIME database existed before the first install,
# so that uninstall.sh can restore the exact previous state.
if [ ! -f "$APP/state" ]; then
    mime_created=0; data_created=0
    [ -e "$DATA/mime" ] || mime_created=1
    [ -e "$DATA" ] || data_created=1
    mkdir -p "$APP"
    printf 'mime_dir_created=%s\ndata_dir_created=%s\n' "$mime_created" "$data_created" > "$APP/state"
fi
declare -a INSTALLED=()
install_file() { # src dest mode
    local src="$1" dest="$2" mode="${3:-644}"
    mkdir -p "$(dirname "$dest")"
    new_tmp "$(dirname "$dest")"
    cp "$src" "$REPLY"
    chmod "$mode" "$REPLY"
    mv -fT "$REPLY" "$dest"   # -T: never into a directory of that name
    INSTALLED+=("$dest")
}
# Paths install.sh creates; uninstall.sh has the same list. Manifest entries
# are only deleted if they match it, so a damaged manifest cannot remove
# anything else.
is_ours() {
    case "$1" in *$'\n'*|*/../*|*/./*|*/..|*/.) return 1 ;; esac
    case "$1" in
        "$APP"/*) no_link_below "$APP" "$1" ;;
        "$DATA/sushi/viewers/niftiPreview.js"|"$DATA/sushi/plugins-1/niftiPreview.js"|\
        "$DATA/mime/packages/gnome-nifti-preview.xml"|"$DATA"/applications/gnome-nifti-preview-*.desktop) return 0 ;;
        *) return 1 ;;
    esac
}
# No folder between ROOT and PATH is a symbolic link: a link planted in our
# folder cannot lead a manifest entry (any name) to a file elsewhere.
no_link_below() { # root path
    local d
    d="$(dirname -- "$2")"
    while [ "$d" != "$1" ]; do
        [ -L "$d" ] && return 1
        case "$d" in "$1"/*) d="$(dirname -- "$d")" ;; *) return 1 ;; esac
    done
}

install_file "$BINARY" "$APP/bin/nifti-preview" 755
for f in "$SRC"/sushi/common/niftiPreview/*.js; do
    install_file "$f" "$APP/gjs/niftiPreview/$(basename "$f")"
done
install_file "$SRC/sushi/legacy/niftiPreview.js" "$DATA/sushi/viewers/niftiPreview.js"
if [ -f "$SRC/sushi/v1/niftiPreview.js" ]; then
    install_file "$SRC/sushi/v1/niftiPreview.js" "$DATA/sushi/plugins-1/niftiPreview.js"
fi
install_file "$SRC/mime/gnome-nifti-preview.xml" "$DATA/mime/packages/gnome-nifti-preview.xml"
install_file "$SRC/uninstall.sh" "$APP/uninstall.sh" 755   # removal without the sources

# ------------------------------------------------------------ "Open With …"
# Sushi's "Open With …" button, like a double-click in Files, uses the default
# application for the file type. Find a NIfTI viewer: a desktop entry that runs
# it, or else its executable (the official ITK-SNAP download has no launcher;
# then add a hidden one).
state_get() { { sed -n "s/^$1=//p" "$APP/state" 2>/dev/null || true; } | head -n1; }
state_set() { # key value
    case "$2" in *$'\n'*) die "invalid state value for $1" ;; esac
    new_tmp "$APP"
    { grep -v "^$1=" "$APP/state" 2>/dev/null || true; printf '%s=%s\n' "$1" "$2"; } > "$REPLY"
    mv -fT "$REPLY" "$APP/state"
}
MIMEAPPS="$CONFIG/mimeapps.list"
NIFTI_TYPES=(image/x-nifti image/x-compressed-nifti)
# Default application when no NIfTI viewer is found (sushi/common/niftiPreview/noViewer.js).
NO_VIEWER_ID=gnome-nifti-preview-no-viewer.desktop
VIEWERS=("ITK-SNAP:itksnap:itksnap itk-snap" "FSLeyes:fsleyes:fsleyes fsl" "MRIcroGL:MRIcroGL:mricrogl" "MRIcron:mricron:mricron")
# One argument of a desktop entry Exec line: quoted, with the characters that
# are special inside quotes escaped, "%" doubled (field codes), then the key
# file's own escaping of backslashes. Control characters cannot be written.
exec_arg() {
    local s="$1"
    case "$s" in *[[:cntrl:]]*) return 1 ;; esac
    s="${s//\\/\\\\}"; s="${s//\"/\\\"}"; s="${s//\`/\\\`}"; s="${s//\$/\\\$}"
    s="\"$s\""
    s="${s//%/%%}"
    printf '%s' "${s//\\/\\\\}"
}
desktop_runs() { # desktop file, executable name: its main entry runs it and is not hidden
    NP_BIN="$2" awk '
        /^\[/ { main = ($0 == "[Desktop Entry]"); next }
        !main { next }
        tolower($0) ~ /^hidden[[:space:]]*=[[:space:]]*true/ { hidden = 1 }
        tolower($0) ~ ("^exec[[:space:]]*=[[:space:]]*(\"?[^ \"]*/)?" tolower(ENVIRON["NP_BIN"]) "(\"| |$)") { runs = 1 }
        END { exit !(runs && !hidden) }' "$1" 2>/dev/null
}
desktop_program() { # desktop file -> the program its main entry runs (first word of Exec)
    awk '
        /^\[/ { main = ($0 == "[Desktop Entry]"); next }
        main && /^Exec[[:space:]]*=/ {
            v = $0; sub(/^Exec[[:space:]]*=[[:space:]]*/, "", v)
            if (v ~ /^"/) { v = substr(v, 2); sub(/".*/, "", v) } else sub(/[[:space:]].*/, "", v)
            print v; exit
        }' "$1" 2>/dev/null
}
find_desktop_entry() { # executable name -> desktop file id that runs it
    local dir f prog
    local -a dirs
    IFS=: read -r -a dirs <<<"$DATA:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
    for dir in "${dirs[@]}"; do
        for f in "$dir"/applications/*.desktop; do
            [ -f "$f" ] || continue
            case "${f##*/}" in gnome-nifti-preview-*) continue ;; esac
            desktop_runs "$f" "$1" || continue
            # Neither the entry nor the program it runs may be changed by another user.
            prog="$(desktop_program "$f")"
            case "$prog" in /*) ;; *) prog="$(command -v -- "$prog" 2>/dev/null)" || continue ;; esac
            trusted_path "$f" && trusted_path "$prog" && { echo "${f##*/}"; return 0; }
        done
    done
    return 1
}
trusted_path() { # file: neither it nor a folder above it can be changed by another user
    local p uid owner mode first=1
    p="$(readlink -f -- "$1")" || return 1
    case "$p" in *[[:cntrl:]]*) return 1 ;; esac   # cannot be written in a desktop entry
    uid="$(id -u)"
    while :; do
        read -r owner mode < <(stat -c '%u %a' -- "$p" 2>/dev/null) || return 1
        [ "$owner" = 0 ] || [ "$owner" = "$uid" ] || return 1
        mode=$((8#$mode))
        if ((mode & 8#022)); then
            ((first)) && return 1            # the program itself is writable by others
            ((mode & 8#1000)) || return 1    # so is a folder above it, without the sticky bit
        fi
        [ "$p" = / ] && return 0
        p="$(dirname -- "$p")"; first=0
    done
}
# GLib looks the program of a launcher up before it turns "%%" back into "%":
# a program whose path has a "%" cannot be launched from a desktop entry.
launchable() { trusted_path "$1" && [[ "$1" != *%* ]]; }
find_executable() { # executable name, folder name patterns -> path
    local bin="$1" base pat p restore
    shift
    p="$(command -v -- "$bin" 2>/dev/null)" && [[ "$p" == /* ]] && launchable "$p" && { echo "$p"; return 0; }
    # Unpacked downloads, e.g. ~/itksnap-4.2.2-20241202-Linux-gcc64/bin/itksnap
    restore="$(shopt -p nullglob nocaseglob)"
    shopt -s nullglob nocaseglob
    for base in "$HOME" "$HOME/opt" "$HOME/Applications" "$HOME/software" "$HOME/.local" /opt /usr/local; do
        for pat in "$@"; do
            for p in "$base"/*"$pat"*/bin/"$bin" "$base"/*"$pat"*/"$bin"; do
                if [ -f "$p" ] && [ -x "$p" ] && launchable "$p"; then
                    eval "$restore"
                    echo "$p"
                    return 0
                fi
            done
        done
    done
    eval "$restore"
    return 1
}
user_chose_default() { # an application for NIfTI files that we did not set, in any file GLib reads
    local f d rc
    local -a files=() desktops=()
    IFS=: read -r -a desktops <<<"${XDG_CURRENT_DESKTOP:-}"
    for d in "${desktops[@]}"; do [ -z "$d" ] || files+=("$CONFIG/${d,,}-mimeapps.list"); done
    files+=("$MIMEAPPS" "$DATA/applications/mimeapps.list" "$DATA/applications/defaults.list")
    for f in "${files[@]}"; do
        [ -e "$f" ] || continue
        [ -r "$f" ] || return 0   # cannot tell: leave the associations alone
        rc=0
        NP_OURS="$(state_get default_app)" awk '
            /^\[/ { group = $0; next }
            group == "[Default Applications]" || group == "[Added Associations]" {
                line = $0; sub(/^[[:space:]]+/, "", line)
                if (line !~ /^image\/x[-.](compressed-)?nifti[[:space:]]*=/) next
                sub(/^[^=]*=[[:space:]]*/, "", line)
                n = split(line, apps, ";")
                for (i = 1; i <= n; i++) {
                    a = apps[i]; gsub(/^[[:space:]]+|[[:space:]]+$/, "", a)
                    if (a != "" && a != ENVIRON["NP_OURS"]) found = 1
                }
            }
            END { exit found ? 0 : 1 }' "$f" || rc=$?
        [ "$rc" = 1 ] || return 0   # found one (0), or could not read it (2)
    done
    return 1
}
mimeapps_ok() { # gio may rewrite mimeapps.list without losing anything
    if [ -L "$MIMEAPPS" ]; then
        warn "$MIMEAPPS is a symbolic link (managed by your dotfiles?): it is left alone."
        warn "To open NIfTI files with ITK-SNAP from the preview, set the default application in Files (Open With…)."
        return 1
    fi
    if [ -e "$MIMEAPPS" ]; then
        if ! [ -f "$MIMEAPPS" ] || ! [ -r "$MIMEAPPS" ] || ! [ -w "$MIMEAPPS" ]; then
            warn "cannot read and write $MIMEAPPS: the default application for NIfTI files is not changed"
            return 1
        fi
        if ! gjs -c 'new imports.gi.GLib.KeyFile().load_from_file(ARGV[0], 0)' "$MIMEAPPS" >/dev/null 2>&1; then
            warn "$MIMEAPPS is not a valid key file: the default application for NIfTI files is not changed"
            return 1
        fi
    fi
    if [ -e "$CONFIG" ] && { ! [ -d "$CONFIG" ] || ! [ -w "$CONFIG" ]; }; then
        warn "cannot write to $CONFIG: the default application for NIfTI files is not changed"
        return 1
    fi
    return 0
}
viewer_id=""; viewer_name=""
if ((DEFAULT_APP)) && command -v gio >/dev/null && ! user_chose_default && mimeapps_ok; then
    for v in "${VIEWERS[@]}"; do
        IFS=: read -r name bin patterns <<<"$v"
        if id="$(find_desktop_entry "$bin")"; then
            viewer_id="$id"; viewer_name="$name"; break
        fi
        # shellcheck disable=SC2086  # patterns is a word list
        if exe="$(find_executable "$bin" $patterns)"; then
            viewer_id="gnome-nifti-preview-${bin,,}.desktop"; viewer_name="$name"
            [ -n "$(state_get applications_dir_created)" ] ||
                state_set applications_dir_created "$([ -e "$DATA/applications" ] && echo 0 || echo 1)"
            say "Found $name: $exe"
            new_tmp "$APP"
            printf '%s\n' "[Desktop Entry]" "Type=Application" "Name=$name" \
                "Comment=Opens NIfTI images (launcher added by GNOME NIfTI Preview)" \
                "Exec=$(exec_arg "$exe") %f" "TryExec=${exe//\\/\\\\}" "Icon=applications-science" "Terminal=false" "NoDisplay=true" \
                "MimeType=image/x-nifti;image/x-compressed-nifti;" "Categories=Science;MedicalSoftware;" > "$REPLY"
            install_file "$REPLY" "$DATA/applications/$viewer_id"
            break
        fi
    done
    # No viewer: a launcher of ours, which explains what to install, is the
    # default. Otherwise the preview (and a double-click in Files) would offer
    # the default application of gzip archives, Files, which extracts the file.
    if [ -z "$viewer_id" ] && command -v gjs >/dev/null; then
        if script="$(exec_arg "$APP/gjs/niftiPreview/noViewer.js")"; then
            viewer_id="$NO_VIEWER_ID"; viewer_name="a NIfTI viewer…"
            [ -n "$(state_get applications_dir_created)" ] ||
                state_set applications_dir_created "$([ -e "$DATA/applications" ] && echo 0 || echo 1)"
            new_tmp "$APP"
            printf '%s\n' "[Desktop Entry]" "Type=Application" "Name=a NIfTI viewer…" "Name[fr]=une visionneuse NIfTI…" \
                "Comment=Explains how to open NIfTI images (added by GNOME NIfTI Preview)" \
                "Exec=gjs $script %f" "TryExec=gjs" "Icon=dialog-information" \
                "Terminal=false" "NoDisplay=true" "MimeType=image/x-nifti;image/x-compressed-nifti;" > "$REPLY"
            install_file "$REPLY" "$DATA/applications/$viewer_id"
        else
            warn "no NIfTI viewer found, and $APP cannot be written in a launcher: Files may offer to extract .nii.gz files; install ITK-SNAP, FSLeyes, MRIcroGL or MRIcron"
        fi
    fi
fi

# A launcher added by an earlier run stays while it may still be the default.
previous="$(state_get default_app)"
case "$previous" in
    gnome-nifti-preview-*.desktop)
        if [ "$previous" != "$viewer_id" ] && [ -f "$DATA/applications/$previous" ]; then
            INSTALLED+=("$DATA/applications/$previous")
        fi ;;
esac

# Remove files a previous version installed but this one no longer ships.
if [ -f "$MANIFEST" ]; then
    while IFS= read -r old; do
        [ -n "$old" ] || continue
        is_ours "$old" || continue
        [ "$old" = "$MANIFEST" ] || [ "$old" = "$APP/state" ] && continue
        keep=0
        for f in "${INSTALLED[@]}"; do [ "$f" = "$old" ] && keep=1 && break; done
        ((keep)) || rm -f -- "$old"
    done < "$MANIFEST"
fi
new_tmp "$APP"
printf '%s\n' "${INSTALLED[@]}" "$MANIFEST" "$APP/state" > "$REPLY"
mv -fT "$REPLY" "$MANIFEST"

update-mime-database "$DATA/mime" >/dev/null 2>&1 || warn "update-mime-database failed"

# Before the first change to mimeapps.list: what uninstall.sh must remove, and
# a copy of the file (gio rewrites it without its comments and blank lines;
# uninstall.sh puts the copy back if nothing else changed the file since).
touched_mimeapps=0
mimeapps_mode=""
note_mimeapps() {
    if [ -z "$mimeapps_mode" ] && [ -f "$MIMEAPPS" ]; then
        mimeapps_mode="$(stat -Lc '%a' -- "$MIMEAPPS")"
    fi
    touched_mimeapps=1
    if [ -n "$(state_get mimeapps_created)" ]; then
        # An unrelated edit since our previous run must survive uninstall,
        # even when this run will modify the association file again.
        if [ ! -f "$MIMEAPPS" ] || [ "$(cksum < "$MIMEAPPS")" != "$(state_get mimeapps_after)" ]; then
            state_set mimeapps_restore_exact 0
        fi
        return
    fi
    state_set config_dir_created "$([ -e "$CONFIG" ] && echo 0 || echo 1)"
    state_set mimeapps_created "$([ -e "$MIMEAPPS" ] && echo 0 || echo 1)"
    if [ -f "$MIMEAPPS" ]; then
        cp -p -- "$MIMEAPPS" "$APP/mimeapps.orig"
    fi
}
if [ -n "$viewer_id" ] && [ "$(state_get default_app)" != "$viewer_id" ]; then
    # Remember what to undo: uninstall.sh removes exactly these entries.
    note_mimeapps
    ok_all=1
    for t in "${NIFTI_TYPES[@]}"; do gio mime "$t" "$viewer_id" >/dev/null 2>&1 || ok_all=0; done
    if ((ok_all)); then
        state_set default_app "$viewer_id"
        state_set default_app_name "$viewer_name"
        if [ "$viewer_id" = "$NO_VIEWER_ID" ]; then
            say "No NIfTI viewer found: the preview's button explains what to install (ITK-SNAP, FSLeyes, MRIcroGL or MRIcron)."
        else
            say "NIfTI files now open with $viewer_name (Files: right-click a file, Open With, to change it)."
        fi
    else
        warn "could not make $viewer_name the default application for NIfTI files"
    fi
fi

# image/x-compressed-nifti is a sub-class of application/gzip: GLib needs it
# to recognise .nii.gz files from a per-user definition. It therefore inherits
# the applications of gzip archives, and the preview would offer "Open With
# Files", which extracts the file. Those inherited applications are hidden for
# NIfTI files ([Removed Associations]; uninstall.sh shows them again), unless
# a NIfTI viewer is the default (the "no viewer" launcher above is not one).
# Applications that declare NIfTI files themselves are kept.
inherited_apps() { # [--hide ID...]: list, or hide, the inherited archive applications (ids, then names)
    gjs -c '
        const Gio = imports.gi.Gio;
        const type = "image/x-compressed-nifti";
        const nifti = ["image/x-nifti", "image/x.nifti", type];
        const hide = ARGV[0] === "--hide" ? ARGV.slice(1) : null;
        const out = [], names = [];
        for (const app of Gio.AppInfo.get_all_for_type(type)) {
            const id = app.get_id();
            if (!id || id.startsWith("gnome-nifti-preview-") || (app.get_supported_types() ?? []).some(t => nifti.includes(t)))
                continue;
            if (!hide || (hide.includes(id) && app.remove_supports_type(type))) {
                out.push(id);
                names.push(app.get_display_name());
            }
        }
        print(out.join(";"));
        print(names.join(", "));' "$@" 2>/dev/null
}
if ((DEFAULT_APP)) && [[ "$(state_get default_app)" == "" || "$(state_get default_app)" == "$NO_VIEWER_ID" ]] &&
    command -v gjs >/dev/null && ! user_chose_default && mimeapps_ok 2>/dev/null; then
    found="$(inherited_apps | head -n1 || true)"
    if [[ -n "$found" && "$found" != *[!A-Za-z0-9._\;-]* ]]; then
        note_mimeapps
        IFS=';' read -r -a ids <<<"$found"
        hidden="$(inherited_apps --hide "${ids[@]}" || true)"
        done_ids="$(head -n1 <<<"$hidden")"
        all="$(printf '%s\n' "$(state_get hidden_apps)" "$done_ids" | tr ';' '\n' | sed '/^$/d' | sort -u | paste -sd';')"
        [ -z "$all" ] || state_set hidden_apps "$all"
        [ -z "$done_ids" ] || say "Opening .nii.gz files with $(sed -n 2p <<<"$hidden") is no longer offered (they would be extracted)."
    fi
fi

# What mimeapps.list looks like after our changes: unchanged at uninstall, the
# copy above replaces it.
if ((touched_mimeapps)) && [ -f "$MIMEAPPS" ]; then
    # GLib replaces this file using mode 0600; preserve the current user's mode.
    [ -z "$mimeapps_mode" ] || chmod "$mimeapps_mode" -- "$MIMEAPPS"
    state_set mimeapps_after "$(cksum < "$MIMEAPPS")"
fi

# Sushi loads viewers when it starts; it exits by itself after 12 s idle.
# Stop only this user's previewer: the owner of its name on our session bus
# (or, without a bus, this user's previewer process).
restart_previewer() {
    local reply pid
    if command -v gdbus >/dev/null &&
        reply="$(gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
            --method org.freedesktop.DBus.GetConnectionUnixProcessID org.gnome.NautilusPreviewer 2>&1)"; then
        pid="$(grep -oE '[0-9]+' <<<"$reply" | tail -n1)"
        [ -n "$pid" ] && [ -O "/proc/$pid" ] && kill "$pid" 2>/dev/null
        return
    fi
    case "${reply:-}" in *NameHasNoOwner*) return 1 ;; esac   # not running
    command -v pkill >/dev/null &&
        pkill -u "$(id -u)" -f '^[^ ]*gjs[^ ]*( -m)? /usr/libexec/org\.gnome\.NautilusPreviewer( |$)' 2>/dev/null
}
if ((RESTART)); then
    restart_previewer && say "Restarted the running previewer." || true
fi

say "GNOME NIfTI Preview installed. Select a .nii or .nii.gz file in Files and press Space."
