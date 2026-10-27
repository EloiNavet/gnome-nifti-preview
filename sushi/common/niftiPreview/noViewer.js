// SPDX-License-Identifier: GPL-2.0-or-later
// GNOME NIfTI Preview — "Open With a NIfTI viewer…" when none is installed.
//
// install.sh makes this the default application for NIfTI files when it finds
// no NIfTI viewer, so that neither the preview's button nor a double-click in
// Files offers the archive tools inherited from gzip (Files would extract the
// file). It explains what to install, then closes.
//
//   gjs noViewer.js FILE            a dialog (GTK 4, or GTK 3)
//   gjs noViewer.js --print FILE    the text only (tests)

const GLib = imports.gi.GLib;

const file = ARGV.filter(a => a !== '--print').pop() ?? '';
const name = file ? GLib.path_get_basename(file) : 'this file';
// Viewers that open each format (by file name).
const FORMATS = [
    [/\.(mgz|mgh|mgh\.gz)$/i, 'FreeSurfer', 'Freeview, FSLeyes or MRIcroGL'],
    [/\.mif(\.gz)?$/i, 'MRtrix', 'MRView or MRIcroGL'],
    [/\.(nrrd|nhdr)$/i, 'NRRD', '3D Slicer, ITK-SNAP or MRIcroGL'],
    [/\.(mha|mhd)$/i, 'MetaImage', '3D Slicer or ITK-SNAP'],
];
const [, kind, viewers] = FORMATS.find(([re]) => re.test(name)) ?? [null, 'NIfTI', 'ITK-SNAP, FSLeyes, MRIcroGL or MRIcron'];
const TITLE = `No ${kind} viewer is installed`;
const BODY = `${viewers} can open “${name}”. Install one, then run the GNOME NIfTI ` +
    'Preview installer again: the preview’s button will open the file in it.\n\n' +
    'Meanwhile, press Space in Files to preview it.';

function main() {
    if (ARGV.includes('--print')) {
        print(`${TITLE}\n${BODY}`);
        return 0;
    }
    let Gtk = null;
    for (const version of ['4.0', '3.0']) {
        try {
            imports.gi.versions.Gtk = version;
            Gtk = imports.gi.Gtk;
            break;
        } catch (e) {}
    }
    if (!Gtk) {
        printerr(`${TITLE}\n${BODY}`);
        return 1;
    }
    const app = new Gtk.Application({application_id: 'org.gnome.NiftiPreview.NoViewer'});
    app.connect('activate', () => {
        const dialog = new Gtk.MessageDialog({
            application: app, modal: true, message_type: Gtk.MessageType.INFO,
            buttons: Gtk.ButtonsType.CLOSE, text: TITLE, secondary_text: BODY,
        });
        dialog.connect('response', () => dialog.destroy());
        if (dialog.present)
            dialog.present();
        else
            dialog.show_all();
    });
    return app.run([]);
}

main();
