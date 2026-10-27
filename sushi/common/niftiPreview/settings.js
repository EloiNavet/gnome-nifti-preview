// Optional user settings, read once per preview from
// ${XDG_CONFIG_HOME:-~/.config}/gnome-nifti-preview/settings.ini (GLib key file).
// A missing file, group or key keeps the default; an invalid value keeps the
// default and logs a warning. Nothing here can make a preview fail.
//
// SCHEMA lists every key once: the previewers read it through load(), the
// Preferences window builds its rows from it, and docs/SETTINGS.md describes
// the same keys.

const {Gio, GLib} = imports.gi;

const MAX_FILE_BYTES = 64 * 1024;

/**
 * Every setting: [group] key, the field name used in code, its type and
 * default, and for the Preferences window a title and a subtitle.
 * Types: choice (values, aliases), bool, int and float (min, max), color
 * (#rrggbb), list (comma-separated values, each one of `values`, in order),
 * path (a file, or empty).
 */
/** Rows of the metadata panel that can be hidden or reordered (see viewState.metadataSections). */
var PANEL_ROWS = Object.freeze([
    {id: 'dimensions', title: 'Dimensions'}, {id: 'volumes', title: 'Volumes'}, {id: 'voxel-size', title: 'Voxel size'},
    {id: 'orientation', title: 'Orientation'}, {id: 'obliquity', title: 'Obliquity'}, {id: 'transform', title: 'Transform'},
    {id: 'datatype', title: 'Datatype'}, {id: 'scaling', title: 'Scaling'}, {id: 'labels', title: 'Labels'},
    {id: 'range', title: 'Range'}, {id: 'window', title: 'Window'}, {id: 'intent', title: 'Intent'},
    {id: 'description', title: 'Description'}, {id: 'format', title: 'Format'}, {id: 'size', title: 'Size'},
    {id: 'details', title: 'Format details'}, {id: 'label-list', title: 'List of labels (segmentations)'},
]);

var SCHEMA = Object.freeze([
    {group: 'display', key: 'orientation', field: 'orientation', type: 'choice', default: 'neurological',
        values: ['neurological', 'radiological', 'stored'],
        title: 'Orientation', subtitle: 'Neurological: left on the left; radiological: left on the right (O switches)'},
    {group: 'display', key: 'palette', field: 'palette', type: 'choice', default: 'gray',
        values: ['gray', 'hot', 'inferno', 'viridis'],
        title: 'Palette', subtitle: 'Colours of intensities (segmentations and RGB images keep theirs)'},
    {group: 'display', key: 'window-low', field: 'windowLow', type: 'float', default: 0.5, min: 0, max: 100, digits: 1,
        title: 'Window low (percentile)', subtitle: 'Values below this percentile are black'},
    {group: 'display', key: 'window-high', field: 'windowHigh', type: 'float', default: 99.5, min: 0, max: 100, digits: 1,
        title: 'Window high (percentile)', subtitle: 'Values above this percentile are white (right-drag on a view to adjust)'},
    {group: 'display', key: 'interpolation', field: 'interpolation', type: 'choice', default: 'auto',
        values: ['auto', 'nearest', 'linear'],
        title: 'Smoothing', subtitle: 'Auto: smooth intensities, sharp segmentations and large voxels'},
    {group: 'display', key: 'axis-labels', field: 'axisLabels', type: 'bool', default: true,
        title: 'Orientation letters', subtitle: 'R/L, A/P, S/I at the edges of the views'},
    {group: 'display', key: 'label-colors', field: 'labelColors', type: 'choice', default: 'auto',
        values: ['auto', 'freesurfer', 'random', 'off'], aliases: {true: 'auto', false: 'off'},
        title: 'Segmentation colours', subtitle: 'FreeSurfer colours and names when recognised; off: grayscale (L switches)'},
    {group: 'display', key: 'label-lut', field: 'labelLut', type: 'path', default: '',
        title: 'Label table', subtitle: 'Colours and names of your own atlas (FreeSurferColorLUT format)'},
    {group: 'display', key: 'crosshair', field: 'crosshair', type: 'choice', default: 'on-click',
        values: ['on-click', 'always', 'never'], aliases: {true: 'always', false: 'never'},
        title: 'Cursor lines', subtitle: 'In the three views: once you click, always or never'},
    {group: 'display', key: 'crosshair-color', field: 'crosshairColor', type: 'color', default: '#61adff',
        title: 'Cursor colour', subtitle: 'Colour of the cursor lines'},
    {group: 'display', key: 'hover', field: 'hover', type: 'bool', default: true,
        title: 'Value under the pointer', subtitle: 'For example "412.5" or "label 51 · 21.6 cm³"'},
    {group: 'display', key: 'hover-coordinates', field: 'hoverCoordinates', type: 'bool', default: false,
        title: 'World coordinates', subtitle: 'Add the position in mm to the value under the pointer'},
    {group: 'display', key: 'label-volume', field: 'labelVolume', type: 'bool', default: true,
        title: 'Label volume', subtitle: 'Volume of the label under the pointer'},
    {group: '4d', key: 'timecourse', field: 'timecourse', type: 'bool', default: true,
        title: 'Time course', subtitle: 'For 4D images, the values of the voxel under the pointer in every volume'},
    {group: '4d', key: 'window', field: 'window4d', type: 'choice', default: 'per-volume', values: ['per-volume', 'first'],
        title: 'Window of each volume', subtitle: 'Per volume (diffusion), or that of the first volume for all (no flicker)'},
    {group: '4d', key: 'start-volume', field: 'startVolume', type: 'choice', default: 'first', values: ['first', 'middle', 'last'],
        title: 'Volume shown first', subtitle: 'Where the slider starts'},
    {group: 'dicom', key: 'series', field: 'dicomSeries', type: 'bool', default: true,
        title: 'Whole series', subtitle: 'A DICOM file opens with the other images of its series (read from its folder)'},
    {group: 'dicom', key: 'window', field: 'dicomWindow', type: 'choice', default: 'file', values: ['file', 'auto'],
        title: 'DICOM window', subtitle: 'The window the file gives, or the automatic one (percentiles)'},
    {group: 'dicom', key: 'show-patient', field: 'showPatient', type: 'bool', default: false,
        title: 'Patient details', subtitle: 'Name, ID, birth date… in the panel (hidden by default, for screenshots)'},
    {group: 'panel', key: 'rows', field: 'panelRows', type: 'list', default: PANEL_ROWS.map(r => r.id),
        values: PANEL_ROWS.map(r => r.id),
        title: 'Rows', subtitle: 'Rows of the metadata panel, in this order'},
    {group: 'panel', key: 'affine', field: 'affine', type: 'bool', default: true,
        title: 'Affine', subtitle: 'The 3 × 4 affine section of the metadata panel'},
    {group: 'thumbnails', key: 'enabled', field: 'thumbnails', type: 'bool', default: true,
        title: 'Thumbnails in Files', subtitle: 'A slice of each image as its icon'},
    {group: 'thumbnails', key: 'view', field: 'thumbnailView', type: 'choice', default: 'axial',
        values: ['axial', 'coronal', 'sagittal'],
        title: 'Thumbnail view', subtitle: 'The centre slice of this view'},
    {group: 'thumbnails', key: 'orientation', field: 'thumbnailOrientation', type: 'choice', default: 'neurological',
        values: ['neurological', 'radiological', 'stored'],
        title: 'Thumbnail orientation', subtitle: 'Display convention of the thumbnails'},
    {group: 'thumbnails', key: 'max-file-mb', field: 'thumbnailMaxFileMb', type: 'int', default: 250, min: 1, max: 65536,
        title: 'Largest compressed file (MB)', subtitle: 'Compressed data to read for one thumbnail; bigger files keep their icon'},
    {group: 'thumbnails', key: 'network', field: 'thumbnailNetwork', type: 'bool', default: false,
        title: 'Files on servers', subtitle: 'Also make thumbnails of files on network file systems (slow: whole images are read)'},
    {group: 'network', key: 'confirm-above-mb', field: 'confirmAboveMb', type: 'int', default: 100, min: 0, max: 65536,
        title: 'Ask above (MB)', subtitle: 'Over the network, ask before reading more than this for one preview (0: never ask)'},
    {group: 'performance', key: 'max-image-mb', field: 'maxImageMb', type: 'int', default: 3072, min: 16, max: 65536,
        title: 'Largest image (MB)', subtitle: 'Voxel data of one volume; larger images show their metadata only'},
    {group: 'performance', key: 'slice-cache-mb', field: 'sliceCacheMb', type: 'int', default: 64, min: 0, max: 4096,
        title: 'Slice cache (MB)', subtitle: 'Memory for slices already drawn, so that scrolling back is instant'},
]);

var DEFAULTS = Object.freeze(Object.fromEntries(SCHEMA.map(e => [e.field, e.default])));

function settingsPath() {
    return GLib.build_filenamev([GLib.get_user_config_dir(), 'gnome-nifti-preview', 'settings.ini']);
}

function _warn(path, message) {
    printerr(`GNOME NIfTI Preview: ${path}: ${message}; using the default.`);
}

/** The key file at `path`, or null (with a warning) when it cannot be read safely. */
function _keyFile(path) {
    if (!GLib.file_test(path, GLib.FileTest.EXISTS))
        return null;
    // Only a small regular file is read: a FIFO would block the previewer.
    try {
        const info = Gio.File.new_for_path(path).query_info('standard::type,standard::size', Gio.FileQueryInfoFlags.NONE, null);
        if (info.get_file_type() !== Gio.FileType.REGULAR || info.get_size() > MAX_FILE_BYTES) {
            _warn(path, `not a regular file of at most ${MAX_FILE_BYTES / 1024} KiB`);
            return null;
        }
    } catch (e) {
        _warn(path, e.message);
        return null;
    }
    const kf = new GLib.KeyFile();
    try {
        kf.load_from_file(path, GLib.KeyFileFlags.KEEP_COMMENTS);
    } catch (e) {
        _warn(path, e.message);
        return null;
    }
    return kf;
}

/** Whether the key file has `key` in `group` (g_key_file_has_key is not introspectable). */
function _has(kf, group, key) {
    try {
        return kf.get_keys(group)[0].includes(key);
    } catch (e) {
        return false; // group missing
    }
}

/** The value of one schema entry in the key file, or undefined (missing or invalid, with a warning). */
function _read(kf, e, path) {
    if (!_has(kf, e.group, e.key))
        return undefined;
    if (e.type === 'choice') {
        let raw;
        try {
            raw = kf.get_string(e.group, e.key).trim();
        } catch (err) {
            _warn(path, `${e.key}: ${err.message}`);
            return undefined;
        }
        const v = e.aliases && Object.hasOwn(e.aliases, raw) ? e.aliases[raw] : raw;
        if (e.values.includes(v))
            return v;
        _warn(path, `${e.key} = ${raw} is not one of ${e.values.join(', ')}`);
        return undefined;
    }
    if (e.type === 'bool') {
        try {
            return kf.get_boolean(e.group, e.key);
        } catch (err) {
            _warn(path, `${e.key} must be true or false`);
            return undefined;
        }
    }
    if (e.type === 'color') {
        let raw = '';
        try {
            raw = kf.get_string(e.group, e.key).trim();
        } catch (err) {
            // reported below
        }
        if (/^#[0-9a-fA-F]{6}$/.test(raw))
            return raw.toLowerCase();
        _warn(path, `${e.key} must be a colour like #61adff`);
        return undefined;
    }
    if (e.type === 'path') {
        try {
            const v = kf.get_string(e.group, e.key).trim();
            if (v === '' || GLib.path_is_absolute(v))
                return v;
        } catch (err) {
            // reported below
        }
        _warn(path, `${e.key} must be an absolute path`);
        return undefined;
    }
    if (e.type === 'list') {
        let items;
        try {
            items = kf.get_string(e.group, e.key).split(',').map(x => x.trim()).filter(Boolean);
        } catch (err) {
            _warn(path, `${e.key}: ${err.message}`);
            return undefined;
        }
        const unknown = items.filter(x => !e.values.includes(x));
        if (unknown.length)
            _warn(path, `${e.key}: ignoring ${unknown.join(', ')} (not one of ${e.values.join(', ')})`);
        return [...new Set(items.filter(x => e.values.includes(x)))];
    }
    let v = null;
    try {
        v = e.type === 'float' ? kf.get_double(e.group, e.key) : kf.get_integer(e.group, e.key);
    } catch (err) {
        // reported below
    }
    if (v !== null && Number.isFinite(v) && v >= e.min && v <= e.max)
        return v;
    _warn(path, e.type === 'float' ? `${e.key} must be a number from ${e.min} to ${e.max}`
        : `${e.key} must be a whole number from ${e.min} to ${e.max}`);
    return undefined;
}

/** Settings merged over DEFAULTS. */
function load(path = settingsPath()) {
    const s = {...DEFAULTS};
    const kf = _keyFile(path);
    if (!kf)
        return s;
    for (const e of SCHEMA) {
        const v = _read(kf, e, path);
        if (v !== undefined)
            s[e.field] = v;
    }
    if (!(s.windowLow < s.windowHigh)) {
        _warn(path, `window-low (${s.windowLow}) must be below window-high (${s.windowHigh})`);
        s.windowLow = DEFAULTS.windowLow;
        s.windowHigh = DEFAULTS.windowHigh;
    }
    return s;
}

/**
 * Write `settings` (every schema field) to `path`, keeping the comments and
 * the unknown keys of an existing file. The file is replaced atomically.
 */
function save(settings, path = settingsPath()) {
    const kf = _keyFile(path) ?? new GLib.KeyFile();
    for (const e of SCHEMA) {
        const v = settings[e.field];
        if (v === undefined)
            continue;
        if (e.type === 'bool')
            kf.set_boolean(e.group, e.key, !!v);
        else if (e.type === 'int')
            kf.set_integer(e.group, e.key, v);
        else if (e.type === 'float')
            kf.set_string(e.group, e.key, String(Number(v)));
        else if (e.type === 'list')
            kf.set_string(e.group, e.key, v.join(', '));
        else
            kf.set_string(e.group, e.key, String(v));
    }
    GLib.mkdir_with_parents(GLib.path_get_dirname(path), 0o755);
    kf.save_to_file(path);
}

/** Display options of the backend's `load` request for these settings. */
function loadOptions(settings) {
    return {
        window_percentiles: [settings.windowLow, settings.windowHigh],
        window_volume: settings.window4d,
        palette: settings.palette,
        label_colors: settings.labelColors === 'off' ? 'auto' : settings.labelColors,
        ...(settings.labelLut ? {label_lut: settings.labelLut} : {}),
        dicom_window: settings.dicomWindow,
    };
}

/** Options of the view state for these settings. */
function stateOptions(settings) {
    return {labelVolume: settings.labelVolume, hoverCoordinates: settings.hoverCoordinates,
        timecourse: settings.timecourse ?? true, panelRows: settings.panelRows, startVolume: settings.startVolume,
        showPatient: settings.showPatient, confirmBytes: settings.confirmAboveMb * 1024 * 1024};
}

/** Options of the backend's `open` request for these settings. */
function openOptions(settings) {
    return {dicom_series: settings.dicomSeries};
}

/** Environment of the backend process for these settings. */
function backendEnv(settings) {
    const mib = 1024 * 1024;
    return {
        NIFTI_PREVIEW_MAX_BYTES: String(settings.maxImageMb * mib),
        NIFTI_PREVIEW_CACHE_BYTES: String(settings.sliceCacheMb * mib),
    };
}
