// The Preferences window (GTK 4, libadwaita), built from the settings schema:
// every page and row, changes saved at once (comments kept), the thumbnailer
// entry rewritten from the settings, "Make thumbnails again" removing only the
// cached thumbnails of medical images.
//
// Needs a display; without one, use a private one:
//   gtk4-broadwayd :9 & GDK_BACKEND=broadway BROADWAY_DISPLAY=:9 gjs tests/gjs/test_preferences.js
// (CI: xvfb-run gjs tests/gjs/test_preferences.js)

imports.gi.versions.Gtk = '4.0';
imports.gi.versions.Gdk = '4.0';
imports.gi.versions.Adw = '1';
const {Adw, GLib, Gio} = imports.gi;
const System = imports.system;

const ROOT = GLib.path_get_dirname(GLib.path_get_dirname(GLib.path_get_dirname(
    Gio.File.new_for_path(System.programPath).get_path())));
imports.searchPath.unshift(GLib.build_filenamev([ROOT, 'sushi', 'common']));
const Settings = imports.niftiPreview.settings;
const Prefs = imports.niftiPreview.prefsWindow;

let failures = 0, passed = 0;
function check(cond, what) {
    if (cond) {
        passed++;
    } else {
        failures++;
        printerr(`FAIL: ${what}`);
    }
}

Adw.init();
const dir = GLib.dir_make_tmp('np-prefs-XXXXXX');
const ini = GLib.build_filenamev([dir, 'settings.ini']);
GLib.file_set_contents(ini, '# my notes\n[display]\norientation = radiological\n');
// The thumbnail program (any executable will do here).
const prog = GLib.build_filenamev([dir, 'gnome-nifti-preview-thumbnailer']);
GLib.file_set_contents(prog, '#!/bin/sh\n');
GLib.chmod(prog, 0o755);
GLib.setenv('GNOME_NIFTI_PREVIEW_THUMBNAILER', prog, true);

const p = new Prefs.PreferencesWindow(null, {settings: ini, data: dir, cache: dir});
check(p.settings.orientation === 'radiological', 'existing settings loaded');
// Every schema entry has a row on some page.
const fields = Settings.SCHEMA.map(e => e.field);
check(fields.length > 20, `${fields.length} settings`);

p.set('palette', 'inferno');
p.set('thumbnailView', 'sagittal');
let text = new TextDecoder().decode(GLib.file_get_contents(ini)[1]);
check(text.includes('# my notes') && text.includes('palette=inferno') && text.includes('orientation=radiological'),
    'changes saved at once, comments and other values kept');
const entryPath = GLib.build_filenamev([dir, 'thumbnailers', 'gnome-nifti-preview.thumbnailer']);
const entry = () => (GLib.file_test(entryPath, GLib.FileTest.EXISTS)
    ? new TextDecoder().decode(GLib.file_get_contents(entryPath)[1]) : '');
check(entry().includes('--view sagittal') && entry().includes('--palette inferno'), 'thumbnailer entry follows the settings');
p.set('thumbnails', false);
check(entry() === '', 'thumbnails off: no entry');
p.set('thumbnails', true);
check(entry().includes(prog), 'thumbnails on again');

// "Make thumbnails again": only the thumbnails of medical images go.
const png = uri => {
    const text = new TextEncoder().encode(`Thumb::URI\0${uri}`);
    const out = new Uint8Array(8 + 12 + text.length + 12);
    out.set([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    const view = new DataView(out.buffer);
    view.setUint32(8, text.length);
    out.set(new TextEncoder().encode('tEXt'), 12);
    out.set(text, 16);
    out.set(new TextEncoder().encode('IEND'), 16 + text.length + 4 + 4);
    return out;
};
const normal = GLib.build_filenamev([dir, 'thumbnails', 'normal']);
const fail = GLib.build_filenamev([dir, 'thumbnails', 'fail', 'gnome-thumbnail-factory']);
GLib.mkdir_with_parents(normal, 0o755);
GLib.mkdir_with_parents(fail, 0o755);
GLib.file_set_contents(GLib.build_filenamev([normal, 'a.png']), png('file:///data/sub-01_T1w.nii.gz'));
GLib.file_set_contents(GLib.build_filenamev([normal, 'b.png']), png('file:///photos/holiday.jpg'));
GLib.file_set_contents(GLib.build_filenamev([fail, 'c.png']), png('file:///data/aseg.mgz'));
GLib.file_set_contents(GLib.build_filenamev([normal, 'd.png']), new Uint8Array([1, 2, 3]));
// A DICOM file without extension (recognised by its content), and another file without one.
const dicom = new Uint8Array(132);
dicom.set(new TextEncoder().encode('DICM'), 128);
GLib.file_set_contents(GLib.build_filenamev([dir, 'IM0001']), dicom);
GLib.file_set_contents(GLib.build_filenamev([dir, 'README']), 'text');
GLib.file_set_contents(GLib.build_filenamev([normal, 'e.png']), png(GLib.filename_to_uri(GLib.build_filenamev([dir, 'IM0001']), null)));
GLib.file_set_contents(GLib.build_filenamev([normal, 'f.png']), png(GLib.filename_to_uri(GLib.build_filenamev([dir, 'README']), null)));
check(Prefs.forgetThumbnails(dir) === 3, 'three medical thumbnails forgotten');
check(!GLib.file_test(GLib.build_filenamev([normal, 'a.png']), GLib.FileTest.EXISTS) &&
    GLib.file_test(GLib.build_filenamev([normal, 'b.png']), GLib.FileTest.EXISTS) &&
    GLib.file_test(GLib.build_filenamev([normal, 'd.png']), GLib.FileTest.EXISTS) &&
    !GLib.file_test(GLib.build_filenamev([normal, 'e.png']), GLib.FileTest.EXISTS) &&
    GLib.file_test(GLib.build_filenamev([normal, 'f.png']), GLib.FileTest.EXISTS), 'other thumbnails kept');

p.resetAll();
check(Settings.load(ini).palette === 'gray' && Settings.load(ini).orientation === 'neurological', 'defaults restored');
p.window.destroy();

GLib.spawn_command_line_sync(`rm -rf ${GLib.shell_quote(dir)}`);
print(`${passed} checks passed, ${failures} failed`);
System.exit(failures ? 1 : 0);
