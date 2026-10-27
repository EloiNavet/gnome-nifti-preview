#!/usr/bin/env gjs
// SPDX-License-Identifier: GPL-2.0-or-later
// GNOME NIfTI Preview — Preferences window (GTK 4, libadwaita).
//   gjs ~/.local/share/gnome-nifti-preview/gjs/niftiPreview/preferences.js
// Opened from the preview's toolbar (gear button) or from the applications
// menu. One window at a time: opening it again shows the one already open.

imports.gi.versions.Gtk = '4.0';
imports.gi.versions.Gdk = '4.0';
imports.gi.versions.Adw = '1';
const {Adw, GLib, Gio} = imports.gi;
const System = imports.system;

// This file's folder is niftiPreview/: its parent holds the shared modules.
imports.searchPath.unshift(GLib.path_get_dirname(GLib.path_get_dirname(
    Gio.File.new_for_path(System.programPath).get_path())));
const {PreferencesWindow} = imports.niftiPreview.prefsWindow;

const app = new Adw.Application({application_id: 'io.github.EloiNavet.GnomeNiftiPreview.Preferences',
    flags: Gio.ApplicationFlags.DEFAULT_FLAGS});
app.connect('activate', () => {
    let win = app.get_active_window();
    if (!win)
        win = new PreferencesWindow(app).window;
    win.present();
});
System.exit(app.run([System.programInvocationName, ...ARGV]));
