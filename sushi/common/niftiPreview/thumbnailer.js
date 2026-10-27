// SPDX-License-Identifier: GPL-2.0-or-later
// nifti-preview: the GNOME thumbnailer entry, written from the settings.
//
// GNOME Files runs thumbnailers in a bubblewrap sandbox that only sees /usr:
// the program must live there (install.sh copies the backend to
// /usr/local/bin with the user's agreement), and it cannot read the settings
// file, so the options are written into the Exec line. install.sh and the
// Preferences window both write the entry through entryText().

const {GLib} = imports.gi;

var FILE_NAME = 'gnome-nifti-preview.thumbnailer';
var SYSTEM_PROGRAM = '/usr/local/bin/gnome-nifti-preview-thumbnailer';
/**
 * Types that get thumbnails (a .nhdr or a .mhd is excluded: its data are in
 * other files the sandbox cannot see; a DICOM file shows its own image, not its series).
 */
var MIME_TYPES = ['image/x-nifti', 'image/x-compressed-nifti', 'image/x-mgh', 'image/x-compressed-mgh', 'image/x-mrtrix',
    'image/x-compressed-mrtrix', 'image/x-nrrd', 'image/x-metaimage', 'application/dicom'];

/** The program GNOME runs (tests and packagers can point elsewhere). */
function program() {
    return GLib.getenv('GNOME_NIFTI_PREVIEW_THUMBNAILER') || SYSTEM_PROGRAM;
}

/** Where the entry goes: ${XDG_DATA_HOME}/thumbnailers/. */
function entryPath(dataDir = GLib.get_user_data_dir()) {
    return GLib.build_filenamev([dataDir, 'thumbnailers', FILE_NAME]);
}

/** One argument of an Exec line (desktop entry quoting). */
function execArg(s) {
    if (/^[A-Za-z0-9_@%+=:,./-]+$/.test(s))
        return s.replace(/%/g, '%%');
    const quoted = `"${s.replace(/[\\"`$]/g, c => `\\${c}`)}"`.replace(/%/g, '%%');
    return quoted.replace(/\\/g, '\\\\');
}

/** The entry for these settings (see settings.js), running `prog`. */
function entryText(settings, prog = program()) {
    const args = ['thumbnail',
        '--view', settings.thumbnailView,
        '--mode', settings.thumbnailOrientation,
        '--colormap', settings.labelColors !== 'off' ? 'auto' : 'rgb',
        '--labels', settings.labelColors === 'off' ? 'auto' : settings.labelColors,
        '--palette', settings.palette,
        '--percentiles', `${settings.windowLow},${settings.windowHigh}`,
        '--max-file-mb', String(settings.thumbnailMaxFileMb),
        ...(settings.thumbnailNetwork ? ['--network'] : [])];
    // %s (size), %i (input path) and %o (output path) are field codes.
    const exec = `${execArg(prog)} ${args.map(execArg).join(' ')} -s %s %i %o`;
    return ['[Thumbnailer Entry]',
        `TryExec=${prog.replace(/\\/g, '\\\\')}`,
        `Exec=${exec}`,
        `MimeType=${MIME_TYPES.join(';')};`,
        ''].join('\n');
}

/**
 * Install or remove the entry for these settings (the program must exist for
 * it to be installed). Returns true when an entry is in place.
 */
function apply(settings, dataDir = GLib.get_user_data_dir(), prog = program()) {
    const path = entryPath(dataDir);
    const wanted = settings.thumbnails && GLib.file_test(prog, GLib.FileTest.IS_EXECUTABLE);
    if (!wanted) {
        if (GLib.file_test(path, GLib.FileTest.EXISTS))
            GLib.unlink(path);
        return false;
    }
    GLib.mkdir_with_parents(GLib.path_get_dirname(path), 0o755);
    GLib.file_set_contents(path, entryText(settings, prog));
    return true;
}
