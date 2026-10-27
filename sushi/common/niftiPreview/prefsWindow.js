// SPDX-License-Identifier: GPL-2.0-or-later
// nifti-preview: the Preferences window (GTK 4, libadwaita), built from the
// settings schema (settings.js). Every change is saved at once to
// settings.ini; the next preview uses it. The thumbnailer entry is rewritten
// when a setting it carries changes.
//
// Legacy GJS module, loaded by preferences.js (and by the tests), with GTK 4.

const {Adw, Gio, GLib, Gtk} = imports.gi;
const Settings = imports.niftiPreview.settings;
const Thumbnailer = imports.niftiPreview.thumbnailer;

/** Pages of the window: title, icon, and the [group] or fields they show. */
const PAGES = [
    {name: 'view', title: 'View', icon: 'view-grid-symbolic', groups: [
        {title: 'Orientation', fields: ['orientation', 'axisLabels']},
        {title: 'Intensities', fields: ['palette', 'windowLow', 'windowHigh', 'interpolation']},
        {title: 'Pointer and cursor', fields: ['hover', 'hoverCoordinates', 'crosshair', 'crosshairColor']},
        {title: 'DICOM', description: 'A file opens with its series; [ and ] go to the other series of the folder.',
            fields: ['dicomSeries', 'dicomWindow', 'showPatient']},
    ]},
    {name: 'labels', title: 'Segmentations', icon: 'applications-graphics-symbolic', groups: [
        {title: 'Segmentations', description: 'Label maps are detected automatically; FreeSurfer parcellations get their colours and names.',
            fields: ['labelColors', 'labelLut', 'labelVolume']},
    ]},
    {name: '4d', title: '4D', icon: 'media-playlist-repeat-symbolic', groups: [
        {title: 'Volumes', description: 'fMRI, diffusion, perfusion: a slider scrubs through the volumes.',
            fields: ['startVolume', 'window4d', 'timecourse']},
    ]},
    {name: 'panel', title: 'Panel', icon: 'view-list-symbolic', groups: [
        {title: 'Sections', fields: ['affine']},
        {title: 'Rows', description: 'Rows of the metadata panel, in this order.', fields: ['panelRows']},
    ]},
    {name: 'thumbnails', title: 'Thumbnails', icon: 'image-x-generic-symbolic', groups: [
        {title: 'Thumbnails in Files', fields: ['thumbnails', 'thumbnailView', 'thumbnailOrientation', 'thumbnailMaxFileMb', 'thumbnailNetwork'],
            extra: 'thumbnails'},
    ]},
    {name: 'performance', title: 'Memory', icon: 'computer-symbolic', groups: [
        {title: 'Memory', description: 'The preview stops reading what goes beyond these.', fields: ['maxImageMb', 'sliceCacheMb']},
        {title: 'Network', description: 'Files on servers (sftp://, smb://, NFS, sshfs) are read in the background, with their progress.',
            fields: ['confirmAboveMb']},
    ]},
];

/** Fields the thumbnailer entry carries (rewritten when one changes). */
const THUMBNAIL_FIELDS = ['thumbnails', 'thumbnailView', 'thumbnailOrientation', 'thumbnailMaxFileMb', 'thumbnailNetwork', 'palette',
    'windowLow', 'windowHigh', 'labelColors'];

/** Human-readable name of a choice value (per field when the same value means different things). */
const CHOICE_NAMES = {
    'neurological': 'Neurological', 'radiological': 'Radiological',
    'stored': 'Stored axes', 'gray': 'Grayscale', 'hot': 'Hot', 'inferno': 'Inferno', 'viridis': 'Viridis',
    'auto': 'Automatic', 'nearest': 'Sharp', 'linear': 'Smooth', 'on-click': 'Once you click',
    'always': 'Always', 'never': 'Never', 'per-volume': 'Per volume', 'first': 'First',
    'middle': 'Middle', 'last': 'Last', 'axial': 'Axial', 'coronal': 'Coronal', 'sagittal': 'Sagittal',
    'freesurfer': 'FreeSurfer', 'random': 'Generic palette', 'off': 'Grayscale',
};
const FIELD_CHOICE_NAMES = {window4d: {first: 'First volume for all'}, labelColors: {auto: 'FreeSurfer when recognised'},
    dicomWindow: {file: 'From the file', auto: 'Automatic (percentiles)'}};

/** File name extensions of the images this project previews (for "Make thumbnails again"). */
var IMAGE_SUFFIXES = ['.nii', '.nii.gz', '.mgh', '.mgz', '.mgh.gz', '.mif', '.mif.gz', '.mih', '.nrrd', '.nhdr', '.mha', '.mhd', '.dcm'];

/**
 * A local file without extension (DICOM files often have none) whose
 * content is one of the types thumbnailed.
 */
function extensionlessImage(uri) {
    const name = uri.slice(uri.lastIndexOf('/') + 1);
    if (!uri.startsWith('file://') || name.includes('.'))
        return false;
    try {
        const type = Gio.File.new_for_uri(uri).query_info('standard::content-type', Gio.FileQueryInfoFlags.NONE, null)
            .get_content_type();
        return Thumbnailer.MIME_TYPES.includes(type);
    } catch (e) {
        return false; // gone, or unreadable
    }
}

/**
 * Remove the cached thumbnails (and remembered failures) of medical images,
 * so that Files makes them again with the current settings. Returns the count.
 */
function forgetThumbnails(cacheDir = GLib.get_user_cache_dir()) {
    let removed = 0;
    const dirs = ['normal', 'large', 'x-large', 'xx-large', 'fail/gnome-thumbnail-factory']
        .map(d => GLib.build_filenamev([cacheDir, 'thumbnails', d]));
    for (const dir of dirs) {
        let children;
        try {
            children = Gio.File.new_for_path(dir).enumerate_children('standard::name,standard::type,standard::size',
                Gio.FileQueryInfoFlags.NOFOLLOW_SYMLINKS, null);
        } catch (e) {
            continue; // no such folder
        }
        let info;
        while ((info = children.next_file(null))) {
            const name = info.get_name();
            if (info.get_file_type() !== Gio.FileType.REGULAR || !name.endsWith('.png') || info.get_size() > 4 << 20)
                continue;
            const path = GLib.build_filenamev([dir, name]);
            const uri = thumbnailUri(path);
            if (uri && (IMAGE_SUFFIXES.some(sfx => uri.toLowerCase().endsWith(sfx)) || extensionlessImage(uri))) {
                try {
                    GLib.unlink(path);
                    removed++;
                } catch (e) {}
            }
        }
        children.close(null);
    }
    return removed;
}

/** The Thumb::URI text chunk of a thumbnail PNG (freedesktop thumbnail spec), or null. */
function thumbnailUri(path) {
    let bytes;
    try {
        bytes = GLib.file_get_contents(path)[1];
    } catch (e) {
        return null;
    }
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    let pos = 8;
    while (pos + 12 <= bytes.length) {
        const len = view.getUint32(pos);
        const type = String.fromCharCode(...bytes.subarray(pos + 4, pos + 8));
        if (pos + 12 + len > bytes.length)
            return null;
        if (type === 'tEXt') {
            const body = bytes.subarray(pos + 8, pos + 8 + len);
            const zero = body.indexOf(0);
            if (zero > 0 && new TextDecoder().decode(body.subarray(0, zero)) === 'Thumb::URI')
                return new TextDecoder().decode(body.subarray(zero + 1));
        }
        if (type === 'IDAT' || type === 'IEND')
            return null; // the text chunks come first
        pos += 12 + len;
    }
    return null;
}

var PreferencesWindow = class PreferencesWindow {
    /**
     * @param app an Adw.Application (or null in tests)
     * @param paths {settings: settings.ini, data: data dir (thumbnailer entry), cache: cache dir}
     */
    constructor(app, paths = {}) {
        this.paths = {settings: Settings.settingsPath(), data: GLib.get_user_data_dir(), cache: GLib.get_user_cache_dir(), ...paths};
        this.settings = Settings.load(this.paths.settings);
        this.window = new Adw.ApplicationWindow({application: app, title: 'GNOME NIfTI Preview', default_width: 640,
            default_height: 720});
        this._toasts = new Adw.ToastOverlay();
        const stack = new Adw.ViewStack({vexpand: true});
        for (const page of PAGES)
            stack.add_titled_with_icon(this._page(page), page.name, page.title, page.icon);
        this._stack = stack;
        // Six pages: icons above their names, so that they fit.
        const header = new Adw.HeaderBar({title_widget: new Adw.ViewSwitcher({stack, policy: Adw.ViewSwitcherPolicy.NARROW})});
        const reset = new Gtk.Button({icon_name: 'edit-undo-symbolic', tooltip_text: 'Back to the default settings'});
        reset.connect('clicked', () => this.resetAll());
        header.pack_start(reset);
        const view = new Adw.ToolbarView({content: stack});
        view.add_top_bar(header);
        this._toasts.set_child(view);
        this.window.set_content(this._toasts);
    }

    /** Store a value, save the file, update the thumbnailer entry if it carries it. */
    set(field, value) {
        this.settings[field] = value;
        try {
            Settings.save(this.settings, this.paths.settings);
        } catch (e) {
            this.toast(`Could not save the settings: ${e.message}`);
            return;
        }
        if (THUMBNAIL_FIELDS.includes(field)) {
            try {
                Thumbnailer.apply(this.settings, this.paths.data);
            } catch (e) {
                this.toast(`Could not update the thumbnails: ${e.message}`);
            }
        }
    }

    toast(text) {
        this._toasts.add_toast(new Adw.Toast({title: text, timeout: 4}));
    }

    resetAll() {
        for (const e of Settings.SCHEMA)
            this.settings[e.field] = Array.isArray(e.default) ? [...e.default] : e.default;
        try {
            Settings.save(this.settings, this.paths.settings);
            Thumbnailer.apply(this.settings, this.paths.data);
        } catch (e) {
            this.toast(`Could not save the settings: ${e.message}`);
        }
        // Rebuild the pages with the new values.
        const visible = this._stack.get_visible_child_name();
        for (const page of PAGES) {
            const old = this._stack.get_child_by_name(page.name);
            this._stack.remove(old);
            this._stack.add_titled_with_icon(this._page(page), page.name, page.title, page.icon);
        }
        this._stack.set_visible_child_name(visible);
        this.toast('Default settings restored');
    }

    _page(page) {
        const p = new Adw.PreferencesPage({title: page.title, icon_name: page.icon});
        for (const g of page.groups) {
            const group = new Adw.PreferencesGroup({title: g.title, description: g.description ?? ''});
            for (const field of g.fields) {
                const e = Settings.SCHEMA.find(x => x.field === field);
                if (e.type === 'list')
                    this._listRows(group, e);
                else
                    group.add(this._row(e));
            }
            if (g.extra === 'thumbnails')
                this._thumbnailRows(group);
            p.add(group);
        }
        return p;
    }

    _row(e) {
        const value = this.settings[e.field];
        const props = {title: e.title, subtitle: e.subtitle};
        if (e.type === 'bool') {
            const row = new Adw.SwitchRow({...props, active: value});
            row.connect('notify::active', r => this.set(e.field, r.active));
            return row;
        }
        if (e.type === 'choice') {
            const model = Gtk.StringList.new(e.values.map(v => FIELD_CHOICE_NAMES[e.field]?.[v] ?? CHOICE_NAMES[v] ?? v));
            const row = new Adw.ComboRow({...props, model, selected: Math.max(0, e.values.indexOf(value))});
            row.connect('notify::selected', r => this.set(e.field, e.values[r.selected]));
            return row;
        }
        if (e.type === 'int' || e.type === 'float') {
            const step = e.type === 'float' ? 0.5 : (e.max > 1000 ? 16 : 1);
            const adjustment = new Gtk.Adjustment({lower: e.min, upper: e.max, step_increment: step, page_increment: step * 10, value});
            const row = new Adw.SpinRow({...props, adjustment, digits: e.digits ?? 0, climb_rate: 1});
            row.connect('notify::value', r => {
                let v = e.type === 'int' ? Math.round(r.value) : Number(r.value.toFixed(e.digits ?? 1));
                // The window percentiles must stay in order.
                if (e.field === 'windowLow' && v >= this.settings.windowHigh)
                    v = Math.max(0, this.settings.windowHigh - 0.5);
                if (e.field === 'windowHigh' && v <= this.settings.windowLow)
                    v = Math.min(100, this.settings.windowLow + 0.5);
                if (v !== r.value)
                    r.value = v;
                this.set(e.field, v);
            });
            return row;
        }
        if (e.type === 'color') {
            const row = new Adw.ActionRow(props);
            const rgba = new imports.gi.Gdk.RGBA();
            rgba.parse(value);
            const button = new Gtk.ColorDialogButton({dialog: new Gtk.ColorDialog({with_alpha: false}), rgba,
                valign: Gtk.Align.CENTER});
            button.connect('notify::rgba', b => {
                const c = b.rgba;
                const hex = [c.red, c.green, c.blue].map(x => Math.round(x * 255).toString(16).padStart(2, '0')).join('');
                this.set(e.field, `#${hex}`);
            });
            row.add_suffix(button);
            row.set_activatable_widget(button);
            return row;
        }
        if (e.type === 'path') {
            const row = new Adw.ActionRow(props);
            const shown = new Gtk.Label({label: value ? GLib.path_get_basename(value) : 'None', css_classes: ['dim-label'],
                ellipsize: 3 /* END */, max_width_chars: 18});
            const choose = new Gtk.Button({icon_name: 'document-open-symbolic', valign: Gtk.Align.CENTER, tooltip_text: 'Choose a file'});
            const clear = new Gtk.Button({icon_name: 'edit-clear-symbolic', valign: Gtk.Align.CENTER, tooltip_text: 'No table',
                sensitive: !!value});
            choose.connect('clicked', () => {
                const dialog = new Gtk.FileDialog({title: e.title});
                dialog.open(this.window, null, (d, res) => {
                    try {
                        const file = d.open_finish(res);
                        if (file?.get_path()) {
                            this.set(e.field, file.get_path());
                            shown.set_label(file.get_basename());
                            clear.set_sensitive(true);
                        }
                    } catch (err) {} // dismissed
                });
            });
            clear.connect('clicked', () => {
                this.set(e.field, '');
                shown.set_label('None');
                clear.set_sensitive(false);
            });
            row.add_suffix(shown);
            row.add_suffix(choose);
            row.add_suffix(clear);
            return row;
        }
        throw new Error(`no row for ${e.type}`);
    }

    /** The panel rows: a switch each, and buttons to move them up and down. */
    _listRows(group, e) {
        const rows = [];
        const rebuild = () => {
            for (const r of rows)
                group.remove(r);
            rows.length = 0;
            const shown = this.settings[e.field];
            const order = [...shown, ...e.values.filter(v => !shown.includes(v))];
            const titles = Object.fromEntries(Settings.PANEL_ROWS.map(r => [r.id, r.title]));
            order.forEach((id, i) => {
                const row = new Adw.SwitchRow({title: titles[id] ?? id, active: shown.includes(id)});
                row.connect('notify::active', r => {
                    const now = order.filter(x => (x === id ? r.active : this.settings[e.field].includes(x)));
                    this.set(e.field, now);
                });
                for (const [icon, delta, tip] of [['go-up-symbolic', -1, 'Move up'], ['go-down-symbolic', 1, 'Move down']]) {
                    const b = new Gtk.Button({icon_name: icon, valign: Gtk.Align.CENTER, tooltip_text: tip,
                        css_classes: ['flat'], sensitive: i + delta >= 0 && i + delta < order.length});
                    b.connect('clicked', () => {
                        const moved = [...order];
                        [moved[i], moved[i + delta]] = [moved[i + delta], moved[i]];
                        // Only the rows shown are saved, in their new order.
                        this.set(e.field, moved.filter(x => this.settings[e.field].includes(x)));
                        rebuild();
                    });
                    row.add_suffix(b);
                }
                group.add(row);
                rows.push(row);
            });
        };
        rebuild();
    }

    /** Whether the program of the thumbnails is there, and a button to make them again. */
    _thumbnailRows(group) {
        const prog = Thumbnailer.program();
        const installed = GLib.file_test(prog, GLib.FileTest.IS_EXECUTABLE);
        const status = new Adw.ActionRow({
            title: installed ? 'Thumbnail program installed' : 'Thumbnail program not installed',
            subtitle: installed ? prog
                : 'GNOME makes thumbnails in a sandbox that only sees /usr: run "install.sh --thumbnails" in a terminal (it asks for your password once).',
        });
        status.add_prefix(new Gtk.Image({icon_name: installed ? 'emblem-ok-symbolic' : 'dialog-information-symbolic'}));
        group.add(status);
        const again = new Adw.ActionRow({title: 'Make thumbnails again',
            subtitle: 'Forget the thumbnails Files keeps for medical images, so that it makes them with these settings'});
        const button = new Gtk.Button({label: 'Refresh', valign: Gtk.Align.CENTER});
        button.connect('clicked', () => {
            const n = forgetThumbnails(this.paths.cache);
            this.toast(n ? `${n} thumbnails will be made again (reopen the folder in Files)` : 'No thumbnails to refresh');
        });
        again.add_suffix(button);
        again.set_activatable_widget(button);
        group.add(again);
    }
};
