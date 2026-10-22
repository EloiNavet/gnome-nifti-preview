// Optional user settings, read once per preview from
// ${XDG_CONFIG_HOME:-~/.config}/gnome-nifti-preview/settings.ini (GLib key file).
// A missing file, group or key keeps the default; an invalid value keeps the
// default and logs a warning. Nothing here can make a preview fail.

const {Gio, GLib} = imports.gi;

const MAX_FILE_BYTES = 64 * 1024;

var DEFAULTS = Object.freeze({
    orientation: 'neurological', // [display] orientation = neurological | radiological | stored
    labelColors: true, //           [display] label-colors: colour segmentations (false: grayscale)
    crosshair: 'on-click', //       [display] crosshair = on-click | always | never
    hover: true, //                 [display] hover: value under the pointer
    hoverCoordinates: false, //     [display] hover-coordinates: add the mm coordinates
    labelVolume: true, //           [display] label-volume: volume of the label under the pointer
    affine: true, //                [panel] affine: the 3×4 affine section
    maxImageMb: 3072, //            [performance] max-image-mb: larger images show their metadata only
    sliceCacheMb: 64, //            [performance] slice-cache-mb: memory for rendered slices
});

function settingsPath() {
    return GLib.build_filenamev([GLib.get_user_config_dir(), 'gnome-nifti-preview', 'settings.ini']);
}

function _warn(path, message) {
    printerr(`GNOME NIfTI Preview: ${path}: ${message}; using the default.`);
}

/** Settings merged over DEFAULTS. */
function load(path = settingsPath()) {
    const s = {...DEFAULTS};
    if (!GLib.file_test(path, GLib.FileTest.EXISTS))
        return s;
    // Only a small regular file is read: a FIFO would block the previewer.
    try {
        const info = Gio.File.new_for_path(path).query_info('standard::type,standard::size', Gio.FileQueryInfoFlags.NONE, null);
        if (info.get_file_type() !== Gio.FileType.REGULAR || info.get_size() > MAX_FILE_BYTES) {
            _warn(path, `not a regular file of at most ${MAX_FILE_BYTES / 1024} KiB`);
            return s;
        }
    } catch (e) {
        _warn(path, e.message);
        return s;
    }
    const kf = new GLib.KeyFile();
    try {
        kf.load_from_file(path, GLib.KeyFileFlags.NONE);
    } catch (e) {
        _warn(path, e.message);
        return s;
    }
    const has = (group, key) => { // (g_key_file_has_key is not introspectable)
        try {
            return kf.get_keys(group)[0].includes(key);
        } catch (e) {
            return false; // group missing
        }
    };
    const choice = (group, key, field, allowed, aliases = {}) => {
        if (!has(group, key))
            return;
        let raw;
        try {
            raw = kf.get_string(group, key).trim();
        } catch (e) {
            _warn(path, `${key}: ${e.message}`);
            return;
        }
        const v = Object.hasOwn(aliases, raw) ? aliases[raw] : raw;
        if (allowed.includes(v))
            s[field] = v;
        else
            _warn(path, `${key} = ${raw} is not one of ${allowed.join(', ')}`);
    };
    const bool = (group, key, field) => {
        if (!has(group, key))
            return;
        try {
            s[field] = kf.get_boolean(group, key);
        } catch (e) {
            _warn(path, `${key} must be true or false`);
        }
    };
    const int = (group, key, field, min, max) => {
        if (!has(group, key))
            return;
        let v = null;
        try {
            v = kf.get_integer(group, key);
        } catch (e) {
            // reported below
        }
        if (v !== null && v >= min && v <= max)
            s[field] = v;
        else
            _warn(path, `${key} must be a whole number from ${min} to ${max}`);
    };
    choice('display', 'orientation', 'orientation', ['neurological', 'radiological', 'stored']);
    bool('display', 'label-colors', 'labelColors');
    choice('display', 'crosshair', 'crosshair', ['on-click', 'always', 'never'], {true: 'always', false: 'never'});
    bool('display', 'hover', 'hover');
    bool('display', 'hover-coordinates', 'hoverCoordinates');
    bool('display', 'label-volume', 'labelVolume');
    bool('panel', 'affine', 'affine');
    int('performance', 'max-image-mb', 'maxImageMb', 16, 65536);
    int('performance', 'slice-cache-mb', 'sliceCacheMb', 0, 4096);
    return s;
}

/** Environment of the backend process for these settings. */
function backendEnv(settings) {
    const mib = 1024 * 1024;
    return {
        NIFTI_PREVIEW_MAX_BYTES: String(settings.maxImageMb * mib),
        NIFTI_PREVIEW_CACHE_BYTES: String(settings.sliceCacheMb * mib),
    };
}
