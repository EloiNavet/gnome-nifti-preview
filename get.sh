#!/usr/bin/env bash
# GNOME NIfTI Preview — one-command installer.
#
#   wget -qO- https://raw.githubusercontent.com/EloiNavet/gnome-nifti-preview/main/get.sh | bash
#   wget -qO- https://raw.githubusercontent.com/EloiNavet/gnome-nifti-preview/main/get.sh | bash -s -- --uninstall
#
# Downloads the latest release (prebuilt backend, nothing to compile), checks
# its SHA-256 and installs it in your home folder. If GNOME Sushi — the
# previewer behind the Space key — is missing, offers to install it first.
#
# Everything is inside main() so that bash reads the whole script before
# running any of it (it arrives through a pipe).
set -euo pipefail
unset CDPATH
tmp=""   # download directory, removed on exit

main() {
    local default_repo="EloiNavet/gnome-nifti-preview"
    local repo="${GNOME_NIFTI_PREVIEW_REPO:-$default_repo}"
    local base="${GNOME_NIFTI_PREVIEW_URL:-https://github.com/$repo/releases/latest/download}"
    local asset="gnome-nifti-preview.tar.gz"

    say() { printf '%s\n' "$*"; }
    die() { printf '\nerror: %s\n' "$*" >&2; exit 1; }

    # It installs into your own home folder: sudo would put root-owned files
    # there (or install for root), and is never needed.
    if [ "$(id -u)" -eq 0 ] && [ "${GNOME_NIFTI_PREVIEW_ALLOW_ROOT:-}" != 1 ]; then
        die "do not run this with sudo or as root: it installs into your home folder. Run it again as yourself."
    fi
    case "${HOME:-}" in /*) ;; *) die "HOME is not set to an absolute path" ;; esac
    [ -O "$HOME" ] || die "your home folder $HOME belongs to another user (sudo -E?); run this as yourself."
    # XDG paths must be absolute; relative ones are ignored (XDG spec).
    local data="${XDG_DATA_HOME:-}"
    case "$data" in /*) ;; *) data="$HOME/.local/share" ;; esac
    local cache="${XDG_CACHE_HOME:-}"
    case "$cache" in /*) ;; *) cache="$HOME/.cache" ;; esac

    if [ "${1:-}" = "--uninstall" ]; then
        if [ -x "$data/gnome-nifti-preview/uninstall.sh" ]; then
            bash "$data/gnome-nifti-preview/uninstall.sh"
        else
            say "GNOME NIfTI Preview is not installed."
        fi
        return
    fi
    [ "$#" -eq 0 ] || die "unknown option: $1 (use --uninstall to remove)"

    case "$(uname -m)" in
        x86_64|aarch64) ;;
        *) die "no prebuilt version for $(uname -m); see the README to build it from source." ;;
    esac

    # ---------------------------------------------------------------- Sushi
    # Any distribution's Sushi package installs this D-Bus service file.
    local service="" d dirs=()
    IFS=: read -r -a dirs <<<"${GNOME_NIFTI_PREVIEW_SERVICE_DIRS:-${XDG_DATA_DIRS:-/usr/local/share:/usr/share}}"
    for d in "${dirs[@]}"; do
        [ -e "$d/dbus-1/services/org.gnome.NautilusPreviewer.service" ] && service=1
    done
    if [ -z "$service" ]; then
        # No -y: the package manager shows what it installs and asks itself.
        local cmd=()
        if command -v apt-get >/dev/null; then cmd=(sudo apt-get install gnome-sushi)
        elif command -v dnf >/dev/null; then cmd=(sudo dnf install sushi)
        elif command -v pacman >/dev/null; then cmd=(sudo pacman -S --needed sushi)
        elif command -v zypper >/dev/null; then cmd=(sudo zypper install sushi)
        else die "GNOME Sushi (the Space-bar previewer of GNOME Files) is not installed; install it with your package manager, then run this again."
        fi
        say "GNOME Sushi, the previewer behind the Space key in Files, is not installed."
        local answer=""
        if [ -r /dev/tty ] && { printf 'Install it now with "%s"? [Y/n] ' "${cmd[*]}" > /dev/tty; read -r answer < /dev/tty; }; then
            case "$answer" in [nN]*) die "GNOME Sushi is required. Install it with: ${cmd[*]}" ;; esac
            "${cmd[@]}" < /dev/tty || die "could not install GNOME Sushi"
        else
            die "GNOME Sushi is required. Install it with: ${cmd[*]}"
        fi
    fi

    # ------------------------------------------------------------- download
    # In the cache folder rather than /tmp, which may be mounted noexec.
    mkdir -p -- "$cache"
    tmp="$(mktemp -d "$cache/gnome-nifti-preview-get.XXXXXX")"
    trap '[ -z "$tmp" ] || rm -rf -- "$tmp"' EXIT
    local secure=1
    if [ "$base" != "https://github.com/$default_repo/releases/latest/download" ]; then
        say "Note: downloading from $base (GNOME_NIFTI_PREVIEW_URL / _REPO is set)."
        secure=0
    fi
    fetch() { # url file
        if command -v curl >/dev/null; then
            if ((secure)); then
                curl -fsSL --proto '=https' --proto-redir '=https' --tlsv1.2 --connect-timeout 20 --max-time 600 --retry 2 -o "$2" "$1"
            else
                curl -fsSL --connect-timeout 20 --max-time 600 --retry 2 -o "$2" "$1"
            fi
        elif command -v wget >/dev/null; then
            if ((secure)); then
                wget -q --https-only --timeout=30 --tries=3 -O "$2" "$1"
            else
                wget -q --timeout=30 --tries=3 -O "$2" "$1"
            fi
        else
            die "neither wget nor curl is installed"
        fi
    }
    say "Downloading GNOME NIfTI Preview..."
    fetch "$base/$asset" "$tmp/$asset" || die "download failed: $base/$asset"
    fetch "$base/$asset.sha256" "$tmp/$asset.sha256" || die "download failed: $base/$asset.sha256"
    # Compare the archive's own hash (sha256sum -c would check whatever file
    # the .sha256 names).
    local want got
    want="$(cut -d' ' -f1 < "$tmp/$asset.sha256")"
    got="$(sha256sum < "$tmp/$asset" | cut -d' ' -f1)"
    [[ "$want" =~ ^[0-9a-f]{64}$ ]] && [ "$got" = "$want" ] ||
        die "the download is corrupted (checksum mismatch); please try again."
    tar -xzf "$tmp/$asset" -C "$tmp" --no-same-owner --no-same-permissions
    local dir
    dir="$(find "$tmp" -mindepth 2 -maxdepth 2 -name install.sh -printf '%h\n' -quit)"
    [ -n "$dir" ] || die "unexpected archive layout"
    if [ ! -e "$dir/bin/$(uname -m)/nifti-preview" ]; then
        die "this release has no prebuilt backend for $(uname -m) yet; see the README to build it from source."
    fi

    bash "$dir/install.sh" --quiet < /dev/null
    say ""
    say "Done. In Files, select a .nii or .nii.gz file and press Space."
    local viewer
    viewer="$(sed -n 's/^default_app_name=//p' "$data/gnome-nifti-preview/state" 2>/dev/null || true)"
    if grep -qx 'default_app=gnome-nifti-preview-no-viewer.desktop' "$data/gnome-nifti-preview/state" 2>/dev/null; then
        say "No NIfTI viewer found: install ITK-SNAP, FSLeyes, MRIcroGL or MRIcron, then run this again to open files in it."
    elif [ -n "$viewer" ]; then
        say "\"Open With $viewer\" in the preview opens the file in $viewer."
    fi
    say "To remove it later:  $data/gnome-nifti-preview/uninstall.sh"
}

main "$@"
