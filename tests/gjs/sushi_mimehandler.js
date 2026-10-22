// Loads the real, installed Sushi 46/50 renderer registry (from its gresource)
// with our viewer present in $XDG_DATA_HOME/sushi/viewers, and checks routing:
// NIfTI types go to GNOME NIfTI Preview, every built-in type keeps its built-in viewer.
//
// Usage: XDG_DATA_HOME=... gjs tests/gjs/sushi_mimehandler.js SUSHI_VERSION [LIBDIR]

const System = imports.system;
const version = ARGV[0] ?? '46.0';
const libdir = ARGV[1] ?? '/usr/lib/x86_64-linux-gnu';

imports.package.init({name: 'sushi', version, prefix: '/usr', libdir});

// Register Sushi's JS resource explicitly (works whatever GJS's package.js
// resource naming convention is) and put it first on the search path.
const {Gio, GLib} = imports.gi;
for (const kind of ['src', 'data']) {
    const file = `/usr/share/sushi/org.gnome.NautilusPreviewer.${kind}.gresource`;
    if (GLib.file_test(file, GLib.FileTest.EXISTS))
        Gio.Resource.load(file)._register();
}
imports.searchPath.unshift('resource:///org/gnome/NautilusPreviewer/js');

// Same GI versions as Sushi's main.js (read from the installed resource).
const mainJs = new TextDecoder().decode(
    Gio.resources_lookup_data('/org/gnome/NautilusPreviewer/js/ui/main.js', 0).toArray());
const requireBlock = mainJs.match(/pkg\.require\((\{[\s\S]*?\})\);/)[1];
pkg.require(eval(`(${requireBlock})`));
imports.gi.Gtk.init(null);

let failures = 0;
function check(cond, what) {
    print(`${cond ? 'ok  ' : 'FAIL'}  ${what}`);
    if (!cond)
        failures++;
}

const MimeHandler = imports.ui.mimeHandler;
const viewers = imports.viewers;
const Fallback = imports.ui.fallbackRenderer.FallbackRenderer;
const ours = viewers.niftiPreview;

check(!!ours && ours.mimeTypes.length === 2, 'GNOME NIfTI Preview viewer loaded by Sushi');
check(MimeHandler.getKlass('image/x-compressed-nifti') === ours.Klass, 'image/x-compressed-nifti -> GNOME NIfTI Preview');
check(MimeHandler.getKlass('image/x-nifti') === ours.Klass, 'image/x-nifti -> GNOME NIfTI Preview');
check(MimeHandler.getKlass('image/png') === viewers.image.Klass, 'image/png -> built-in image viewer');
check(MimeHandler.getKlass('image/jpeg') === viewers.image.Klass, 'image/jpeg -> built-in image viewer');
check(MimeHandler.getKlass('application/pdf') === viewers.evince.Klass, 'application/pdf -> built-in evince viewer');
check(MimeHandler.getKlass('text/plain') === viewers.text.Klass, 'text/plain -> built-in text viewer');
check(MimeHandler.getKlass('text/x-python') === viewers.text.Klass, 'text/x-python -> built-in text viewer');
check(MimeHandler.getKlass('application/gzip') === Fallback, 'application/gzip -> fallback (unchanged)');
check(MimeHandler.getKlass('application/x-compressed-tar') === Fallback, 'application/x-compressed-tar -> fallback (unchanged)');

System.exit(failures ? 1 : 0);
