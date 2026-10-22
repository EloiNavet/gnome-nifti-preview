#!/usr/bin/env bash
# GNOME NIfTI Preview — removes everything install.sh put in your home directory.
#   ./uninstall.sh [--no-restart] [--quiet]
set -euo pipefail
unset CDPATH

die() { printf 'error: %s\n' "$*" >&2; exit 1; }
if [ "$(id -u)" -eq 0 ] && [ "${GNOME_NIFTI_PREVIEW_ALLOW_ROOT:-}" != 1 ]; then
    die "do not run this with sudo or as root: GNOME NIfTI Preview is installed in your own home folder."
fi
case "${HOME:-}" in /*) ;; *) die "HOME is not set to an absolute path" ;; esac
[ -O "$HOME" ] || die "your home folder $HOME belongs to another user (sudo -E?); run this as yourself."
# XDG paths must be absolute; relative ones are ignored (as install.sh does).
DATA="${XDG_DATA_HOME:-}"
case "$DATA" in /*) ;; *) DATA="$HOME/.local/share" ;; esac
CONFIG="${XDG_CONFIG_HOME:-}"
case "$CONFIG" in /*) ;; *) CONFIG="$HOME/.config" ;; esac
APP="$DATA/gnome-nifti-preview"
RESTART=1
[ "${GNOME_NIFTI_PREVIEW_NO_RESTART:-}" != 1 ] || RESTART=0   # tests
QUIET=0
while (($#)); do
    case "$1" in
        --no-restart) RESTART=0; shift ;;
        --quiet) QUIET=1; shift ;;
        -h|--help) sed -n '2,3p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 64 ;;
    esac
done
say() { ((QUIET)) || printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
TMPS=()
cleanup() { if ((${#TMPS[@]})); then rm -f -- "${TMPS[@]}"; fi; }
trap cleanup EXIT

# Paths install.sh creates (same list as there): manifest entries are only
# deleted if they match it, so a damaged manifest cannot remove anything else.
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

mime_dir_created=0; data_dir_created=0; mimeapps_created=0; config_dir_created=0; applications_dir_created=0
default_app=""; hidden_apps=""; mimeapps_after=""
if [ -f "$APP/state" ]; then
    grep -qx 'mime_dir_created=1' "$APP/state" && mime_dir_created=1
    grep -qx 'data_dir_created=1' "$APP/state" && data_dir_created=1
    grep -qx 'mimeapps_created=1' "$APP/state" && mimeapps_created=1
    grep -qx 'config_dir_created=1' "$APP/state" && config_dir_created=1
    default_app="$(sed -n 's/^default_app=//p' "$APP/state" | head -n1)"
    mimeapps_after="$(sed -n 's/^mimeapps_after=//p' "$APP/state" | head -n1)"
    hidden_apps="$(sed -n 's/^hidden_apps=//p' "$APP/state" | head -n1)"
    grep -qx 'applications_dir_created=1' "$APP/state" && applications_dir_created=1
fi

# Undo the default application install.sh set for NIfTI files and show again
# the archive applications it hid for them (and only that: a choice the user
# made since then is kept).
MIMEAPPS="$CONFIG/mimeapps.list"
# Write through a symbolic link (dotfile managers), keeping the file mode.
target="$(readlink -f -- "$MIMEAPPS" 2>/dev/null || true)"
if [ -n "$default_app$hidden_apps" ] && [ -n "$target" ] && [ -f "$target" ] && ! { [ -r "$target" ] && [ -w "$target" ] && [ -w "$(dirname -- "$target")" ]; }; then
    warn "cannot rewrite $MIMEAPPS: remove the image/x-nifti and image/x-compressed-nifti lines yourself"
elif [ -n "$default_app$hidden_apps" ] && [ -n "$target" ] && [ -f "$target" ] && [ -n "$mimeapps_after" ] &&
    [ "$(cksum < "$target")" = "$mimeapps_after" ] &&
    ! grep -qx 'mimeapps_restore_exact=0' "$APP/state" &&
    if ((mimeapps_created)); then ! [ -L "$MIMEAPPS" ]; else [ -f "$APP/mimeapps.orig" ]; fi; then
    # Nothing changed it since the install: put back the file exactly as it
    # was (or remove it, if the install created it).
    if ((mimeapps_created)); then
        rm -f -- "$MIMEAPPS"
        if ((config_dir_created)); then rmdir -- "$CONFIG" 2>/dev/null || true; fi
    else
        tmp="$(mktemp "$target.XXXXXX")"; TMPS+=("$tmp")
        cat -- "$APP/mimeapps.orig" > "$tmp"
        chmod --reference="$target" -- "$tmp" 2>/dev/null || true
        mv -fT -- "$tmp" "$target"
    fi
elif [ -n "$default_app$hidden_apps" ] && [ -n "$target" ] && [ -f "$target" ]; then
    tmp="$(mktemp "$target.XXXXXX")"; TMPS+=("$tmp")
    NP_ID="$default_app" NP_HIDDEN=";$hidden_apps;" awk '
        function filtered(group, line,   key, v, n, apps, i, out, ours) {
            if (group == "[Removed Associations]") {
                if (line !~ /^image\/x-compressed-nifti=/)
                    return line
            } else if ((group != "[Default Applications]" && group != "[Added Associations]") ||
                line !~ /^image\/x-(compressed-)?nifti=/)
                return line
            key = line; sub(/=.*/, "", key); v = line; sub(/^[^=]*=/, "", v)
            n = split(v, apps, ";"); out = ""
            for (i = 1; i <= n; i++) {
                if (group == "[Removed Associations]")
                    ours = index(ENVIRON["NP_HIDDEN"], ";" apps[i] ";") > 0
                else  # the default set, or one of our launchers (an earlier default)
                    ours = apps[i] == ENVIRON["NP_ID"] || apps[i] ~ /^gnome-nifti-preview-[A-Za-z0-9._-]*\.desktop$/
                if (apps[i] != "" && !ours)
                    out = out apps[i] ";"
            }
            if (out == "")
                return "\001"
            if (group == "[Default Applications]" && v !~ /;$/)
                sub(/;$/, "", out)
            return key "=" out
        }
        FNR == NR {  # first pass: which groups had lines, which still have some
            if ($0 ~ /^\[/) group = $0
            else {
                if ($0 !~ /^[[:space:]]*$/) had[group] = 1
                if ((l = filtered(group, $0)) != "\001" && l !~ /^[[:space:]]*$/) keys[group] = 1
            }
            next
        }
        # A group is dropped only if our lines were all it had.
        /^\[/ { group = $0; skip = (group in had) && !(group in keys); if (!skip) out[++n] = $0; next }
        !skip { l = filtered(group, $0); if (l != "\001") out[++n] = l }
        END { while (n > 0 && out[n] ~ /^[[:space:]]*$/) n--; for (i = 1; i <= n; i++) print out[i] }
    ' "$target" "$target" > "$tmp" || { warn "could not read $MIMEAPPS; it is left unchanged"; tmp=""; }
    if [ -z "$tmp" ]; then
        :
    elif ((mimeapps_created)) && ! [ -L "$MIMEAPPS" ] && ! grep -q '[^[:space:]]' "$tmp"; then
        rm -f -- "$tmp" "$MIMEAPPS"
        if ((config_dir_created)); then rmdir -- "$CONFIG" 2>/dev/null || true; fi
    elif cmp -s "$tmp" "$target"; then
        rm -f -- "$tmp"
    else
        chmod --reference="$target" -- "$tmp" 2>/dev/null || true
        mv -fT -- "$tmp" "$target"
    fi
fi
files=(
    "$DATA/sushi/viewers/niftiPreview.js"
    "$DATA/sushi/plugins-1/niftiPreview.js"
    "$DATA/mime/packages/gnome-nifti-preview.xml"
)
if [ -f "$APP/manifest.txt" ]; then
    while IFS= read -r f; do
        if is_ours "$f"; then files+=("$f"); fi
    done < "$APP/manifest.txt"
fi
for f in "${files[@]}"; do
    if [ -d "$f" ] && ! [ -L "$f" ]; then continue; fi   # never a directory (rm -f would fail)
    rm -f -- "$f"
done
rm -rf -- "$APP"

# Remove directories only if this left them empty.
for d in "$DATA/sushi/viewers" "$DATA/sushi/plugins-1" "$DATA/sushi"; do
    rmdir -- "$d" 2>/dev/null || true
done
if ((applications_dir_created)); then rmdir -- "$DATA/applications" 2>/dev/null || true; fi

if [ -d "$DATA/mime" ]; then
    if [ -n "$(ls -A "$DATA/mime/packages" 2>/dev/null)" ] || ((!mime_dir_created)); then
        # Other user MIME definitions exist (or the database predates us): rebuild it.
        mkdir -p "$DATA/mime/packages"
        update-mime-database "$DATA/mime" >/dev/null 2>&1 || true
        rmdir -- "$DATA/mime/packages" 2>/dev/null || true
    else
        # install.sh created this generated database and nothing else uses it.
        rm -rf -- "$DATA/mime"
    fi
fi

# install.sh created the data directory itself (fresh account): remove it if empty.
if ((data_dir_created)); then
    rmdir -- "$DATA" 2>/dev/null && [ "$DATA" = "$HOME/.local/share" ] && rmdir -- "$HOME/.local" 2>/dev/null || true
fi

# Stop only this user's previewer (see install.sh).
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
    restart_previewer || true
fi
say "GNOME NIfTI Preview removed."
