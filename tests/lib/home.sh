# shellcheck shell=bash
# Sourced by the shell tests. Throw-away home folders that keep the real one
# out of reach, whatever the developer's environment exports.

# DIR becomes HOME, with every XDG base directory inside it.
use_home() {
    [ -n "${1:-}" ] && [ -d "$1" ] || { echo "use_home: no directory" >&2; exit 1; }
    export HOME="$1"
    export XDG_DATA_HOME="$1/.local/share" XDG_CONFIG_HOME="$1/.config" \
        XDG_CACHE_HOME="$1/.cache" XDG_STATE_HOME="$1/.local/state"
}

# The installers refuse to run as root, which test containers often are, and
# must never restart the developer's own previewer.
[ "$(id -u)" != 0 ] || export GNOME_NIFTI_PREVIEW_ALLOW_ROOT=1
export GNOME_NIFTI_PREVIEW_NO_RESTART=1

# Under GitHub Actions a failure is also an annotation, readable without the
# raw log; so is a command that stops a script (set -e).
ci_error() {
    [ -n "${GITHUB_ACTIONS:-}" ] || return 0
    printf '::error::%s\n' "$(printf '%s' "$1" | sed -e ':a' -e 'N' -e '$!ba' -e 's/%/%25/g; s/\r/%0D/g; s/\n/%0A/g')"
}
if [ -n "${GITHUB_ACTIONS:-}" ]; then
    set -E
    trap 'ci_error "${BASH_SOURCE[0]##*/} line $LINENO: \"$BASH_COMMAND\" failed"' ERR
fi
