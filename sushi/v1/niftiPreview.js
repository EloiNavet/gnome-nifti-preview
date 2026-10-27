// SPDX-License-Identifier: GPL-2.0-or-later
// GNOME NIfTI Preview — NIfTI previewer for GNOME Sushi 51+ (GTK 4, plugin API v1).
// EXPERIMENTAL: tested with Sushi 51.0 under Xvfb only.
//
// Installed as ~/.local/share/sushi/plugins-1/niftiPreview.js. The backend client
// and the view state are the same legacy modules as for Sushi 46–50; ES
// modules can still load them through the global `imports` object.

import Adw from 'gi://Adw';
import Gdk from 'gi://Gdk';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Gio from 'gi://Gio';
import Graphene from 'gi://Graphene';
import Gsk from 'gi://Gsk';
import Gtk from 'gi://Gtk';

import {Renderer, ResizePolicy} from 'resource://org/gnome/NautilusPreviewer/plugin-api-1.js';

const SHARED_DIR = GLib.build_filenamev([GLib.get_user_data_dir(), 'gnome-nifti-preview', 'gjs']);
const DEBUG = !!GLib.getenv('NIFTI_PREVIEW_DEBUG');
// The side panel appears once, complete. Statistics normally arrive before
// the first slices; if they take longer than this, the panel is shown with
// placeholders for them.
const PANEL_WAIT_MS = 1000;
// Voxels bigger than this on screen (pixels) are drawn sharp, not smoothed.
const SHARP_FROM = 6;

const CSS = `
.np-view { background-color: #000; }
.np-edge { color: rgba(255, 255, 255, 0.9); font-weight: bold; margin: 6px; text-shadow: 0 0 3px #000; }
.np-title { color: rgba(255, 255, 255, 0.75); font-size: smaller; margin: 6px 8px; text-shadow: 0 0 3px #000; }
.np-title.np-active { color: #99c1f1; font-weight: bold; }
.np-meta { padding: 14px 16px; }
.np-meta-title { font-weight: bold; font-size: larger; }
.np-section { font-weight: bold; font-size: smaller; margin-top: 10px; opacity: 0.6; }
.np-key { opacity: 0.6; }
.np-mono { font-family: monospace; font-size: smaller; }
.np-warning { font-size: smaller; }
.np-volume { padding: 4px 12px 2px 12px; }
.np-volume-text { font-size: smaller; font-feature-settings: "tnum"; }
`;

let cssInstalled = false;
function installCss() {
    if (cssInstalled)
        return;
    const provider = new Gtk.CssProvider();
    provider.load_from_string(CSS);
    Gtk.StyleContext.add_provider_for_display(Gdk.Display.get_default(), provider,
        Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION);
    cssInstalled = true;
}

function loadShared() {
    if (!imports.searchPath.includes(SHARED_DIR))
        imports.searchPath.push(SHARED_DIR);
    try {
        return [imports.niftiPreview.backendClient, imports.niftiPreview.viewState, imports.niftiPreview.settings,
            imports.niftiPreview.spectrumPlot, imports.niftiPreview.timecoursePlot];
    } catch (e) {
        throw new Error(`GNOME NIfTI Preview is not fully installed (${e.message}); run install.sh again.`);
    }
}

/**
 * Screen geometry of a slice: offset and scale of image pixels in a w×h area.
 * Pixel sizes that are not finite and positive count as 1, and their ratio is
 * bounded, so that a hostile header never produces a degenerate transform.
 * Returns null when nothing can be drawn.
 */
function sliceGeometry(plan, w, h) {
    const size = x => (Number.isFinite(x) && x > 0 ? x : 1);
    let pw = size(plan.pixel_w), ph = size(plan.pixel_h);
    if (pw / ph > 1000)
        pw = ph * 1000;
    else if (ph / pw > 1000)
        ph = pw * 1000;
    const s = Math.min(w / (plan.width * pw), h / (plan.height * ph));
    const sx = s * pw, sy = s * ph;
    if (!(Number.isFinite(sx) && Number.isFinite(sy) && sx > 0 && sy > 0))
        return null;
    return [(w - plan.width * sx) / 2, (h - plan.height * sy) / 2, sx, sy];
}

function label(text, classes = [], props = {}) {
    return new Gtk.Label({label: text, css_classes: classes, ...props});
}

class SliceCanvas extends Gtk.Widget {
    static {
        GObject.registerClass({
            GTypeName: 'NiftiPreviewSliceCanvas',
            Signals: {
                'scroll-slice': {param_types: [GObject.TYPE_INT]},
                // Shift + wheel: the volume of a 4D image.
                'scroll-volume': {param_types: [GObject.TYPE_INT]},
                'pick': {param_types: [GObject.TYPE_DOUBLE, GObject.TYPE_DOUBLE]},
                // Image pixel under the pointer, (-1, -1) when it leaves the image.
                'hover': {param_types: [GObject.TYPE_DOUBLE, GObject.TYPE_DOUBLE]},
                // Right button: drag (dx, dy) adjusts the window; a double click resets it.
                'window-start': {},
                'window-drag': {param_types: [GObject.TYPE_DOUBLE, GObject.TYPE_DOUBLE]},
                'window-reset': {},
            },
        }, this);
    }

    constructor() {
        super({hexpand: true, vexpand: true, accessible_role: Gtk.AccessibleRole.IMG});
        this._texture = null;
        this._plan = null;
        this._cross = null;
        this._nearest = false;
        this._accum = 0;
        this._readout = '';
        this._pointer = null;
        this.interpolation = 'auto';
        this.crossColour = [0.38, 0.68, 1.0];

        const scroll = new Gtk.EventControllerScroll({flags: Gtk.EventControllerScrollFlags.BOTH_AXES});
        scroll.connect('scroll', (c, dx, dy) => {
            const shift = (c.get_current_event_state() & Gdk.ModifierType.SHIFT_MASK) !== 0;
            // Some systems turn Shift + wheel into horizontal scrolling.
            this._accum -= dy || (shift ? dx : 0);
            const steps = Math.trunc(this._accum);
            this._accum -= steps;
            if (steps)
                this.emit(shift ? 'scroll-volume' : 'scroll-slice', steps);
            return true;
        });
        this.add_controller(scroll);

        const drag = new Gtk.GestureDrag({button: Gdk.BUTTON_PRIMARY});
        drag.connect('drag-begin', (g, x, y) => this._pick(x, y));
        drag.connect('drag-update', (g, dx, dy) => {
            const [, x, y] = g.get_start_point();
            this._pick(x + dx, y + dy);
        });
        this.add_controller(drag);

        const windowDrag = new Gtk.GestureDrag({button: Gdk.BUTTON_SECONDARY});
        windowDrag.connect('drag-begin', () => this.emit('window-start'));
        windowDrag.connect('drag-update', (g, dx, dy) => this.emit('window-drag', dx, dy));
        this.add_controller(windowDrag);
        const windowReset = new Gtk.GestureClick({button: Gdk.BUTTON_SECONDARY});
        windowReset.connect('pressed', (g, n) => {
            if (n === 2)
                this.emit('window-reset');
        });
        this.add_controller(windowReset);

        const motion = new Gtk.EventControllerMotion();
        motion.connect('motion', (c, x, y) => {
            const px = this._imagePixel(x, y);
            this._pointer = px ? [x, y] : null;
            this.emit('hover', px ? px[0] : -1, px ? px[1] : -1);
            if (this._readout)
                this.queue_draw();
        });
        motion.connect('leave', () => {
            this._pointer = null;
            this.emit('hover', -1, -1);
        });
        this.add_controller(motion);
    }

    /** Text drawn next to the pointer ('' hides it). */
    setReadout(text) {
        if (text === this._readout)
            return;
        this._readout = text;
        this.queue_draw();
    }

    /** Accessible name and description (screen readers). */
    describe(name, description) {
        if (name === this._a11yName && description === this._a11yDescription)
            return;
        this._a11yName = name;
        this._a11yDescription = description;
        this.update_property([Gtk.AccessibleProperty.LABEL, Gtk.AccessibleProperty.DESCRIPTION], [name, description]);
    }

    setPlan(plan) {
        this._plan = plan;
        this.queue_draw();
    }

    setImage(reply, nearest) {
        this._texture = Gdk.MemoryTexture.new(reply.width, reply.height, Gdk.MemoryFormat.R8G8B8,
            new GLib.Bytes(reply.pixels), reply.width * 3);
        this._nearest = nearest;
        this.queue_draw();
    }

    setCrosshair(cross) {
        this._cross = cross;
        this.queue_draw();
    }

    release() {
        this._texture = null;
        this._plan = null;
    }

    _geometry() {
        return sliceGeometry(this._plan, this.get_width(), this.get_height());
    }

    _imagePixel(x, y) {
        if (!this._plan || !this._texture)
            return null;
        const g = this._geometry();
        if (!g)
            return null;
        const [ox, oy, sx, sy] = g;
        const col = (x - ox) / sx;
        const row = (y - oy) / sy;
        return col >= 0 && row >= 0 && col < this._plan.width && row < this._plan.height ? [col, row] : null;
    }

    _pick(x, y) {
        const px = this._imagePixel(x, y);
        if (px)
            this.emit('pick', px[0], px[1]);
    }

    _snapshotReadout(snapshot, w, h) {
        if (!this._readout || !this._pointer)
            return;
        const layout = this.create_pango_layout(this._readout);
        const [, logical] = layout.get_pixel_extents();
        const pad = 5;
        const bw = logical.width + 2 * pad;
        const bh = logical.height + 2 * pad - 2;
        let [x, y] = this._pointer;
        x = Math.round(Math.max(2, x + 14 + bw > w ? x - 8 - bw : x + 14));
        y = Math.round(Math.max(2, y + 18 + bh > h ? y - 8 - bh : y + 18));
        const box = new Gsk.RoundedRect().init_from_rect(new Graphene.Rect().init(x, y, bw, bh), 5);
        snapshot.push_rounded_clip(box);
        snapshot.append_color(new Gdk.RGBA({red: 0, green: 0, blue: 0, alpha: 0.75}), box.bounds);
        snapshot.pop();
        snapshot.save();
        snapshot.translate(new Graphene.Point({x: x + pad, y: y + pad - 1}));
        snapshot.append_layout(layout, new Gdk.RGBA({red: 1, green: 1, blue: 1, alpha: 0.95}));
        snapshot.restore();
    }

    vfunc_snapshot(snapshot) {
        try {
            this._snapshot(snapshot);
        } catch (e) {
            logError(e, 'nifti-preview: snapshot');
        }
    }

    _snapshot(snapshot) {
        const w = this.get_width();
        const h = this.get_height();
        const black = new Gdk.RGBA({red: 0, green: 0, blue: 0, alpha: 1});
        snapshot.append_color(black, new Graphene.Rect().init(0, 0, w, h));
        const g = this._texture && this._plan ? this._geometry() : null;
        if (!g)
            return;
        const [ox, oy, sx, sy] = g;
        const rect = new Graphene.Rect().init(ox, oy, this._plan.width * sx, this._plan.height * sy);
        // Labels never get interpolated colours; intensities are smoothed,
        // unless a voxel is large on screen (then it stays sharp).
        const sharp = this.interpolation === 'nearest' ||
            (this.interpolation === 'auto' && (this._nearest || Math.min(sx, sy) >= SHARP_FROM)) ||
            (this.interpolation === 'linear' && this._nearest);
        snapshot.append_scaled_texture(this._texture, sharp ? Gsk.ScalingFilter.NEAREST : Gsk.ScalingFilter.LINEAR, rect);
        if (this._cross) {
            const [red, green, blueLevel] = this.crossColour;
            const blue = new Gdk.RGBA({red, green, blue: blueLevel, alpha: 0.7});
            const x = Math.round(ox + (this._cross[0] + 0.5) * sx);
            const y = Math.round(oy + (this._cross[1] + 0.5) * sy);
            snapshot.append_color(blue, new Graphene.Rect().init(x, oy, 1, this._plan.height * sy));
            snapshot.append_color(blue, new Graphene.Rect().init(ox, y, this._plan.width * sx, 1));
        }
        this._snapshotReadout(snapshot, w, h);
    }
}

class SliceView extends Gtk.Overlay {
    static {
        GObject.registerClass({GTypeName: 'NiftiPreviewSliceView'}, this);
    }

    constructor(view) {
        super({hexpand: true, vexpand: true, css_classes: ['np-view']});
        this.view = view;
        this.canvas = new SliceCanvas();
        this.set_child(this.canvas);
        const edge = (h, v) => label('', ['np-edge'], {halign: h, valign: v, can_target: false});
        this._labels = {
            left: edge(Gtk.Align.START, Gtk.Align.CENTER),
            right: edge(Gtk.Align.END, Gtk.Align.CENTER),
            top: edge(Gtk.Align.CENTER, Gtk.Align.START),
            bottom: edge(Gtk.Align.CENTER, Gtk.Align.END),
        };
        this._title = label('', ['np-title'], {halign: Gtk.Align.START, valign: Gtk.Align.START, can_target: false});
        this._note = label('', ['np-title'], {halign: Gtk.Align.CENTER, valign: Gtk.Align.CENTER,
            can_target: false, visible: false});
        for (const l of [...Object.values(this._labels), this._title, this._note])
            this.add_overlay(l);
        this._plan = null;
    }

    setPlan(plan, index) {
        this._plan = plan;
        this.canvas.setPlan(plan);
        for (const side of ['left', 'right', 'top', 'bottom'])
            this._labels[side].set_label(plan.labels[side]);
        this.setIndex(index);
    }

    setIndex(index) {
        if (this._plan)
            this._title.set_label(`${this._plan.title}  ${index} / ${this._plan.n_slices - 1}`);
    }

    /** Display settings: orientation letters, smoothing, cursor colour (#rrggbb). */
    configure({axisLabels = true, interpolation = 'auto', crosshairColor = '#61adff'} = {}) {
        for (const l of Object.values(this._labels))
            l.set_visible(axisLabels);
        this.canvas.interpolation = interpolation;
        const m = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(crosshairColor);
        if (m)
            this.canvas.crossColour = m.slice(1).map(h => parseInt(h, 16) / 255);
        this.canvas.queue_draw();
    }

    /** The view Page Up/Down act on, once chosen with the keyboard (V). */
    setActive(active) {
        if (active)
            this._title.add_css_class('np-active');
        else
            this._title.remove_css_class('np-active');
    }

    showNote(text) {
        this._note.set_label(text);
        this._note.set_visible(true);
    }

    hideNote() {
        this._note.set_visible(false);
    }
}

export const Klass = class NiftiPreviewRenderer extends Adw.Bin {
    static {
        GObject.registerClass({
            GTypeName: 'NiftiPreviewRenderer',
            Implements: [Renderer],
        }, this);
    }

    constructor(file, fileInfo, constructProperties = {}) {
        super(constructProperties);
        installCss();
        this._file = file;
        this._t0 = GLib.get_monotonic_time();
        this._shown = new Set();
        this._state = null;
        this._client = null;
        this._error = null;
        this._hovered = 'axial';
        this._keyboardView = false; // V pressed: the active view is shown
        this._keyController = null;
        this._buttons = {};
        this._handlers = [];

        this._buildGrid();
        const overlay = new Gtk.Overlay({child: this._grid});
        this._spinner = new Gtk.Spinner({spinning: true, width_request: 32, height_request: 32,
            halign: Gtk.Align.CENTER, valign: Gtk.Align.CENTER, can_target: false});
        overlay.add_overlay(this._spinner);
        this._overlay = overlay;
        this.set_child(overlay);

        this._toolbar = this._buildToolbar();
        this._track(this, this.connect('map', () => this._hookKeys()));
        // Ready right away, like Sushi's own viewers: the window is sized and shown
        // immediately (with a spinner) instead of after the backend answered.
        this.markReady();
        // Started from an idle, so that a renderer dropped right away (before
        // Sushi kept a reference to stop it) never starts a backend.
        this._startIdle = GLib.idle_add(GLib.PRIORITY_DEFAULT, () => {
            this._startIdle = 0;
            if (!this.cancellable.is_cancelled() && this._metaBox)
                this._start().catch(e => this._fail(e));
            return GLib.SOURCE_REMOVE;
        });
    }

    /** The views and the side panel (built again for another DICOM series). */
    _buildGrid() {
        const grid = new Gtk.Grid({row_homogeneous: true, column_homogeneous: true,
            row_spacing: 1, column_spacing: 1, hexpand: true, vexpand: true});
        this._grid = grid;
        this._plot = null; //     spectroscopy: SpectrumPlot and its drawing area
        this._plotArea = null;
        this._views = {};
        const positions = {coronal: [0, 0], sagittal: [1, 0], axial: [0, 1]};
        for (const [view, [x, y]] of Object.entries(positions)) {
            const v = new SliceView(view);
            this._track(v.canvas, v.canvas.connect('scroll-slice', (w, steps) => this._state?.scroll(view, steps)));
            this._track(v.canvas, v.canvas.connect('scroll-volume', (w, steps) => this._state?.stepVolume(steps)));
            this._track(v.canvas, v.canvas.connect('pick', (w, col, row) => this._pick(view, col, row)));
            this._track(v.canvas, v.canvas.connect('hover', (w, col, row) => {
                if (this._settings?.hover)
                    this._state?.hover(view, col < 0 ? null : col, row);
            }));
            // Right-drag: window of the grey levels (intensities only).
            this._track(v.canvas, v.canvas.connect('window-start', () => {
                this._windowStart = this._state?.displayWindow ?? this._state?.stats?.window ?? null;
            }));
            this._track(v.canvas, v.canvas.connect('window-drag', (w, dx, dy) => {
                if (this._state?.effectiveColormap === 'gray')
                    this._state.dragWindow(this._windowStart, dx, dy);
            }));
            this._track(v.canvas, v.canvas.connect('window-reset', () => {
                if (this._state?.effectiveColormap === 'gray')
                    this._state.setWindow(null);
            }));
            const motion = new Gtk.EventControllerMotion();
            this._track(motion, motion.connect('enter', () => {
                this._hovered = view;
                this._showActive();
            }));
            v.add_controller(motion);
            grid.attach(v, x, y, 1, 1);
            this._views[view] = v;
        }
        this._metaBox = new Gtk.Box({orientation: Gtk.Orientation.VERTICAL, spacing: 2, css_classes: ['np-meta']});
        // Horizontal scrolling keeps the (non-wrapping) affine from imposing a minimum width.
        const scroller = new Gtk.ScrolledWindow({hscrollbar_policy: Gtk.PolicyType.AUTOMATIC, child: this._metaBox,
            hexpand: true, vexpand: true});
        this._scroller = scroller;
        // The fourth quarter: the series menu and the 4D slider (when there
        // are), above the panel.
        this._side = new Gtk.Box({orientation: Gtk.Orientation.VERTICAL, spacing: 0, hexpand: true, vexpand: true});
        this._side.append(scroller);
        grid.attach(this._side, 1, 1, 1, 1);
        this._volumeScale = this._volumeLabel = this._timecourse = this._timecourseArea = null;
        this._seriesRow = this._seriesDrop = null;
        this._loadButton = null;
        this._singleView = null;
    }

    get resizePolicy() {
        return ResizePolicy.MAX_SIZE;
    }

    /** Remember a signal connection so that cleanup() can break the closure cycle. */
    _track(object, id) {
        this._handlers.push([object, id]);
        return id;
    }

    get toolbar() {
        return this._toolbar;
    }

    _debug(what) {
        if (DEBUG)
            printerr(`nifti-preview: ${what} after ${((GLib.get_monotonic_time() - this._t0) / 1000).toFixed(1)} ms`);
    }

    async _start() {
        const path = this._file.get_path();
        if (!path)
            throw new Error(`This ${this._file.get_uri_scheme()}:// location has no local path: install gvfs-fuse (GNOME's bridge that gives remote files a path, often the gvfs-fuse package), or mount the server with sshfs or NFS.`);
        const [Client, State, Settings, Plot, Timecourse] = loadShared();
        this._settings = Settings.load();
        this._State = State;
        this._Plot = Plot;
        this._Timecourse = Timecourse;
        try {
            this._client = new Client.BackendClient({env: Settings.backendEnv(this._settings)});
        } catch (e) {
            throw new Error(`The GNOME NIfTI Preview backend could not be started (${e.message}). Run install.sh again.`);
        }
        this._Settings = Settings;
        this._state = this._newState();
        this._crossVisible = this._settings.crosshair === 'always';
        for (const v of Object.values(this._views))
            v.configure(this._settings);
        try {
            // Without label colours: grayscale, except the colours of RGB images.
            await this._state.open(path, this._settings.orientation, this._colormap(),
                Settings.loadOptions(this._settings), Settings.openOptions(this._settings));
        } catch (e) {
            if (this.cancellable.is_cancelled() || e.code === 'closed')
                return;
            if (this._state.header) {
                this._showMessage(e.message);
                return;
            }
            if (e.about) {
                this._showAbout(path, e);
                return;
            }
            throw e;
        }
    }

    _colormap() {
        return this._settings.labelColors !== 'off' ? 'auto' : 'rgb';
    }

    _newState() {
        return new this._State.ViewState(this._client, {
            onHeader: s => this._onHeader(s),
            onStats: s => {
                this._debug('statistics received');
                this._refresh(s);
            },
            onSlice: (view, reply) => this._onSlice(view, reply),
            onCursor: s => this._onCursor(s),
            onReading: s => this._onReading(s),
            onSpectrum: s => this._onSpectrum(s),
            onVolume: s => this._onVolume(s),
            onTimecourse: s => this._onTimecourse(s),
            onLabels: s => this._fillMetadata(s),
            onError: e => this._onError(e),
            onSeries: s => this._onSeries(s),
            onLoadState: s => this._onLoadState(s),
        }, this._Settings.stateOptions(this._settings));
    }

    /**
     * DICOM: show another series of the folder (or DICOMDIR). The same
     * backend opens it; the views and the panel are built again.
     */
    async _switchSeries(uid) {
        if (!this._metaBox || !this._client || this._switching || uid === this._state?.header?.dicom?.series_uid)
            return;
        this._switching = true;
        const mode = this._state?.mode ?? this._settings.orientation;
        try {
            this._state?.destroy();
            if (this._panelTimer) {
                GLib.source_remove(this._panelTimer);
                this._panelTimer = 0;
            }
            for (const v of Object.values(this._views))
                v.canvas.release();
            this._plotArea?.set_draw_func(null);
            this._timecourseArea?.set_draw_func(null);
            this._buildGrid();
            this._overlay.set_child(this._grid);
            for (const v of Object.values(this._views))
                v.configure(this._settings);
            this._error = null;
            this._spinner?.set_visible(true);
            this._state = this._newState();
            await this._state.openSeries(uid, mode, this._colormap(), this._Settings.loadOptions(this._settings));
        } catch (e) {
            if (!this.cancellable.is_cancelled() && e.code !== 'closed')
                this._showMessage(e.message);
        } finally {
            this._switching = false;
        }
    }

    /** [ and ]: the previous or next series. */
    _stepSeries(step) {
        const list = this._seriesList?.series ?? [];
        const i = list.findIndex(x => x.uid === this._state?.header?.dicom?.series_uid);
        const next = list[i + step];
        if (i < 0 || !next)
            return false;
        this._switchSeries(next.uid);
        return true;
    }

    /** Network: what the views say until the data are read, and the button to read them. */
    _onLoadState(state) {
        if (!this._metaBox)
            return;
        const text = this._State.remoteText(state);
        for (const v of Object.values(this._views)) {
            if (text)
                v.showNote(text);
            else
                v.hideNote();
        }
        if (text)
            this._spinner?.set_visible(false);
        if (state.remote?.confirm && !this._loadButton) {
            const b = new Gtk.Button({label: `Load the image (${this._State.formatBytes(state.remote.bytes)} over the network)`,
                halign: Gtk.Align.CENTER, margin_top: 6, margin_bottom: 6, css_classes: ['suggested-action']});
            this._track(b, b.connect('clicked', () => {
                if (this._loadButton)
                    this._side.remove(this._loadButton);
                this._loadButton = null;
                this._state?.confirmLoad().catch(e => this._onError(e));
            }));
            this._loadButton = b;
            this._side.insert_child_after(b, this._seriesRow);
        } else if (!state.remote?.confirm && this._loadButton) {
            this._side.remove(this._loadButton);
            this._loadButton = null;
        }
    }

    _onSeries(state) {
        if (state.seriesList)
            this._seriesList = state.seriesList;
        this._buildSeriesRow(state);
    }

    /** The series menu, at the top of the side panel, when there are other series. */
    _buildSeriesRow(state) {
        const list = this._seriesList?.series ?? [];
        if (!this._metaBox || list.length < 2 || this._seriesRow)
            return;
        const studies = new Set(list.map(x => x.study_uid)).size;
        const row = new Gtk.Box({orientation: Gtk.Orientation.HORIZONTAL, spacing: 8, css_classes: ['np-volume']});
        row.append(label('Series', ['np-key'], {valign: Gtk.Align.CENTER}));
        const names = list.map(x => this._State.seriesLabel(x, studies > 1));
        const drop = Gtk.DropDown.new_from_strings(names);
        // The button cuts long names, so that the menu never widens the
        // preview; the list shows them whole.
        const factory = ellipsize => {
            const f = new Gtk.SignalListItemFactory();
            this._track(f, f.connect('setup', (fa, item) => {
                item.set_child(new Gtk.Label({xalign: 0, ellipsize: ellipsize ? 3 : 0, width_chars: ellipsize ? 12 : -1}));
            }));
            this._track(f, f.connect('bind', (fa, item) => item.get_child().set_label(item.get_item().get_string())));
            return f;
        };
        drop.set_factory(factory(true));
        drop.set_list_factory(factory(false));
        drop.set_hexpand(true);
        drop.set_focusable(false);
        drop.set_tooltip_text(`${list.length} series in this ${this._seriesList.origin === 'dicomdir' ? 'DICOMDIR' : 'folder'} ([ and ] keys)`);
        const current = list.findIndex(x => x.uid === state.header?.dicom?.series_uid);
        if (current >= 0)
            drop.set_selected(current);
        this._track(drop, drop.connect('notify::selected', d => {
            const pick = list[d.get_selected()];
            if (pick)
                this._switchSeries(pick.uid);
        }));
        row.append(drop);
        this._seriesRow = row;
        this._seriesDrop = drop;
        this._side.prepend(row);
    }

    /**
     * A single slice (a radiograph, an ultrasound clip, one DICOM image):
     * its view alone on the left, the panel on the right.
     */
    _setupSingleImage(state) {
        const view = this._State.singleView(state);
        if (!view || this._singleView || !this._views[view] || !this._grid)
            return;
        this._singleView = view;
        for (const [name, w] of Object.entries(this._views)) {
            if (name !== view) {
                w.canvas.release();
                this._grid.remove(w);
                delete this._views[name];
            }
        }
        const kept = this._views[view];
        this._grid.remove(kept);
        this._grid.remove(this._side);
        this._grid.attach(kept, 0, 0, 1, 2);
        this._grid.attach(this._side, 1, 0, 1, 2);
        this._hovered = view;
    }

    _fail(error) {
        if (this.cancellable.is_cancelled() || error?.code === 'closed')
            return;
        if (error?.name === 'BackendError')
            printerr(`nifti-preview: ${error.code}: ${error.message}`);
        else
            logError(error, 'nifti-preview');
        // A renderer that fails before being shown is never mapped, so Sushi
        // will not call cleanup(): release the backend now.
        this.cleanup();
        // Sushi 51 shows this text in an Adw.StatusPage, which parses markup.
        this.markFailed(new GLib.Error(Gio.IOErrorEnum, Gio.IOErrorEnum.FAILED,
            GLib.markup_escape_text(`Cannot preview this file: ${error.message}`, -1)));
    }

    stop() {
        // Called by Sushi when the preview is dismissed, possibly before it was shown.
        this.cleanup();
    }

    _onError(error) {
        if (error?.code !== 'closed' && !this.cancellable.is_cancelled())
            this._showMessage(error.message);
    }

    /**
     * A file recognised but without slices (a DICOM report, a surface
     * overlay): the panel alone, with what the backend tells of it.
     */
    _showAbout(path, error) {
        if (!this._metaBox || !this._grid)
            return; // cleaned up
        this._about = true;
        this._spinner?.set_visible(false);
        // A layout of its own: the panel takes the whole window.
        for (const w of Object.values(this._views))
            w.canvas.release();
        this._views = {};
        this._metaBox = new Gtk.Box({orientation: Gtk.Orientation.VERTICAL, spacing: 2, css_classes: ['np-meta']});
        this._scroller = new Gtk.ScrolledWindow({hscrollbar_policy: Gtk.PolicyType.AUTOMATIC, child: this._metaBox,
            hexpand: true, vexpand: true});
        this._grid = new Gtk.Grid({hexpand: true, vexpand: true});
        this._grid.attach(this._scroller, 0, 0, 1, 1);
        this._overlay.set_child(this._grid);
        this._metaBox.append(label(GLib.path_get_basename(path), ['np-meta-title'], {xalign: 0, wrap: true,
            wrap_mode: 2 /* WORD_CHAR */, selectable: true, tooltip_text: path}));
        this._metaBox.append(label(error.message, ['np-warning'], {xalign: 0, wrap: true}));
        this._metaBox.append(this._sectionsGrid(this._State.aboutSections(error.about)));
        this._updateButtons();
    }

    /** Sections two by two; a report's text takes a row of its own. */
    _sectionsGrid(sections) {
        const grid = new Gtk.Grid({column_spacing: 28, row_spacing: 4, column_homogeneous: true});
        let cell = 0;
        for (const sec of sections) {
            const box = new Gtk.Box({orientation: Gtk.Orientation.VERTICAL, spacing: 3});
            box.append(label(sec.title.toUpperCase(), ['np-section'], {xalign: 0}));
            if (sec.text !== undefined) {
                box.append(label(sec.text, sec.prose ? [] : ['np-mono'], {xalign: 0, selectable: true,
                    wrap: !!sec.prose, wrap_mode: 2 /* WORD_CHAR */}));
            } else {
                const rows = new Gtk.Grid({column_spacing: 10, row_spacing: 2});
                sec.rows.forEach(([key, value], row) => {
                    rows.attach(label(key, ['np-key'], {xalign: 0, valign: Gtk.Align.START}), 0, row, 1, 1);
                    rows.attach(label(value, [], {xalign: 0, selectable: true, wrap: true, wrap_mode: 2 /* WORD_CHAR */,
                        max_width_chars: 22}), 1, row, 1, 1);
                });
                box.append(rows);
            }
            if (sec.prose) {
                cell += cell % 2;
                grid.attach(box, 0, cell / 2, 2, 1);
                cell += 2;
            } else {
                grid.attach(box, cell % 2, Math.floor(cell / 2), 1, 1);
                cell++;
            }
        }
        return grid;
    }

    _showMessage(text) {
        if (!this._metaBox)
            return; // cleaned up
        this._error = text;
        this._spinner?.set_visible(false);
        for (const v of Object.values(this._views))
            v.showNote('Image data unavailable');
        if (this._plot) {
            this._plot.setMessage('Spectrum unavailable');
            this._plotArea.queue_draw();
        }
        if (this._state?.header)
            this._fillMetadata(this._state);
    }

    /**
     * Spectroscopy: a spectrum plot. A single voxel has no views: the plot
     * takes the top row and the panel the bottom row. Spectroscopic imaging
     * keeps its maps; the plot of the cursor voxel sits above the panel.
     */
    _setupSpectroscopy(state) {
        if (this._plot || !state.header?.mrs || !this._grid)
            return;
        this._plot = new this._Plot.SpectrumPlot();
        const area = new Gtk.DrawingArea({hexpand: true, vexpand: true, css_classes: ['np-view'], accessible_role: Gtk.AccessibleRole.IMG});
        area.set_draw_func((a, cr, w, h) => {
            try {
                this._plot?.draw(cr, w, h, t => a.create_pango_layout(t));
            } catch (e) {
                logError(e, 'nifti-preview: spectrum');
            } finally {
                cr.$dispose();
            }
        });
        const motion = new Gtk.EventControllerMotion();
        this._track(motion, motion.connect('motion', (c, x) => {
            this._plot.pointer = x;
            area.queue_draw();
        }));
        this._track(motion, motion.connect('leave', () => {
            this._plot.pointer = null;
            area.queue_draw();
        }));
        area.add_controller(motion);
        this._plotArea = area;
        this._grid.remove(this._side);
        if (state.header.mrs.single_voxel) {
            for (const v of Object.values(this._views)) {
                v.canvas.release();
                this._grid.remove(v);
            }
            this._views = {};
            this._grid.attach(area, 0, 0, 2, 1);
            this._grid.attach(this._side, 0, 1, 2, 1);
        } else {
            const box = new Gtk.Box({orientation: Gtk.Orientation.VERTICAL, spacing: 1, homogeneous: true});
            box.append(area);
            box.append(this._side);
            this._grid.attach(box, 1, 1, 1, 1);
        }
        this._updatePlotTitle(state);
    }

    _updatePlotTitle(state) {
        if (!this._plot || !state?.header)
            return;
        const m = state.header.mrs;
        const what = this._plot.part === 'magnitude' ? 'magnitude' : 'real part';
        const voxel = state.spectrum?.voxel;
        const combined = m.combined ?? m.transients;
        this._plot.title = m.single_voxel
            ? `${m.nucleus ?? ''} spectrum · ${combined > 1 ? `${combined} transients combined · ` : ''}${what}`.trim()
            : `Voxel ${voxel ? voxel.join(', ') : '…'} · ${what}`;
        this._plotArea?.update_property([Gtk.AccessibleProperty.LABEL, Gtk.AccessibleProperty.DESCRIPTION],
            ['Spectrum', `${this._plot.title}; ${this._plot.rangeLabel()}${this._plot.message ? `; ${this._plot.message}` : ''}`]);
    }

    _onSpectrum(state) {
        if (!this._plot || !this._metaBox)
            return;
        if (state.header.mrs.single_voxel)
            this._spinner?.set_visible(false);
        if (state.spectrum && !this._plot.spectrum)
            this._debug('spectrum shown');
        if (state.spectrum)
            this._plot.setSpectrum(state.spectrum);
        else
            this._plot.setMessage(state.spectrumError ?? 'Spectrum unavailable');
        this._updatePlotTitle(state);
        this._plotArea.queue_draw();
        this._updateButtons();
    }

    _togglePart() {
        if (!this._plot?.spectrum)
            return;
        this._plot.togglePart();
        this._updatePlotTitle(this._state);
        this._plotArea.queue_draw();
        this._updateButtons();
    }

    _toggleRange() {
        if (!this._plot?.hasRange())
            return;
        this._plot.toggleRange();
        this._updatePlotTitle(this._state);
        this._plotArea.queue_draw();
        this._updateButtons();
    }

    /**
     * A 4D image: above the panel, a slider that scrubs through the volumes
     * and the time course of the voxel under the pointer (or the cursor).
     * Clicking or dragging on the time course picks a volume.
     */
    _setupVolumes(state) {
        if (this._volumeScale || !state.browsable() || !this._grid)
            return;
        const n = state.header.n_volumes;
        const box = new Gtk.Box({orientation: Gtk.Orientation.VERTICAL, spacing: 0});
        const row = new Gtk.Box({orientation: Gtk.Orientation.HORIZONTAL, spacing: 10, css_classes: ['np-volume']});
        const adjustment = new Gtk.Adjustment({lower: 0, upper: n - 1, step_increment: 1,
            page_increment: Math.max(1, Math.round(n / 10)), value: state.volumeTarget});
        // Not focusable: the arrow keys keep switching files in Sushi.
        this._volumeScale = new Gtk.Scale({orientation: Gtk.Orientation.HORIZONTAL, adjustment, draw_value: false,
            digits: 0, round_digits: 0, hexpand: true, focusable: false,
            tooltip_text: 'Volume (Shift + wheel on a view, comma / period, Home / End)'});
        this._track(this._volumeScale, this._volumeScale.connect('value-changed', scale => {
            if (!this._settingScale)
                this._state?.setVolume(scale.get_value());
        }));
        this._volumeLabel = label('', ['np-volume-text'], {xalign: 1, width_chars: 18});
        row.append(this._volumeScale);
        row.append(this._volumeLabel);
        box.append(row);
        if (this._settings.timecourse ?? true) {
            this._timecourse = new this._Timecourse.TimecoursePlot();
            const area = new Gtk.DrawingArea({hexpand: true, height_request: 110, css_classes: ['np-view'],
                accessible_role: Gtk.AccessibleRole.IMG});
            area.update_property([Gtk.AccessibleProperty.LABEL], ['Time course']);
            area.set_draw_func((a, cr, w, h) => {
                try {
                    this._timecourse?.draw(cr, w, h, t => a.create_pango_layout(t));
                } catch (e) {
                    logError(e, 'nifti-preview: time course');
                } finally {
                    cr.$dispose();
                }
            });
            const pickVolume = x => {
                const k = this._timecourse?.volumeAt(x, area.get_width());
                if (k !== null && k !== undefined)
                    this._state?.setVolume(k);
            };
            const drag = new Gtk.GestureDrag({button: Gdk.BUTTON_PRIMARY});
            this._track(drag, drag.connect('drag-begin', (g, x) => pickVolume(x)));
            this._track(drag, drag.connect('drag-update', (g, dx) => {
                const [, x] = g.get_start_point();
                pickVolume(x + dx);
            }));
            area.add_controller(drag);
            const motion = new Gtk.EventControllerMotion();
            this._track(motion, motion.connect('motion', (c, x) => {
                this._timecourse.pointer = x;
                area.queue_draw();
            }));
            this._track(motion, motion.connect('leave', () => {
                this._timecourse.pointer = null;
                area.queue_draw();
            }));
            area.add_controller(motion);
            this._timecourseArea = area;
            box.append(area);
        }
        this._side.insert_child_after(box, this._seriesRow);
        this._onVolume(state);
    }

    _onVolume(state) {
        if (!this._volumeScale || !this._metaBox)
            return;
        this._settingScale = true;
        try {
            if (Math.round(this._volumeScale.get_value()) !== state.volumeTarget)
                this._volumeScale.set_value(state.volumeTarget);
        } finally {
            this._settingScale = false;
        }
        this._volumeLabel.set_label(this._State.volumeText(state));
        if (this._timecourse) {
            this._timecourse.setVolume(state.volume, state.volumeTarget);
            this._timecourseArea.queue_draw();
        }
    }

    _onTimecourse(state) {
        if (!this._timecourse || !this._metaBox)
            return;
        this._timecourse.setTimecourse(state.timecourse);
        this._timecourseArea.queue_draw();
    }

    _onHeader(state) {
        this._setupSingleImage(state);
        this._setupSpectroscopy(state);
        this._setupVolumes(state);
        // DICOM: the other series of the folder, asked for once.
        if (state.header.dicom) {
            if (this._seriesList)
                this._buildSeriesRow(state);
            else if (!this._seriesAsked) {
                this._seriesAsked = true;
                state.requestSeries();
            }
        }
        for (const [view, w] of Object.entries(this._views))
            w.setPlan(state.plan(view), state.sliceIndex(view));
        this._onCursor(state);
        if (state.stats || this._error)
            this._fillMetadata(state);
        else if (!this._panelTimer)
            this._panelTimer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, PANEL_WAIT_MS, () => {
                this._panelTimer = 0;
                if (this._metaBox && this._state?.header && !this._state.stats && !this._error)
                    this._fillMetadata(this._state, true);
                return GLib.SOURCE_REMOVE;
            });
        this._updateButtons();
    }

    _refresh(state) {
        this._fillMetadata(state);
        this._updateButtons();
    }

    _onSlice(view, reply) {
        const plan = this._state?.plan(view);
        if (!this._views?.[view] || !plan || reply.view !== view || reply.width !== plan.width || reply.height !== plan.height)
            return;
        this._spinner?.set_visible(false);
        this._views[view].canvas.setImage(reply, reply.colormap === 'labels');
        this._views[view].setIndex(reply.index);
        if (DEBUG && !this._shown.has(view)) {
            this._shown.add(view);
            if (this._shown.size === 3)
                this._debug('three slices shown');
        }
    }

    _pick(view, col, row) {
        // The crosshair appears once the user starts navigating (crosshair = on-click).
        if (this._settings?.crosshair === 'on-click')
            this._crossVisible = true;
        this._state?.pick(view, col, row);
    }

    _onCursor(state) {
        for (const [view, w] of Object.entries(this._views)) {
            w.setIndex(state.sliceIndex(view));
            w.canvas.setCrosshair(this._crossVisible ? state.crosshair(view) : null);
        }
        this._describeViews(state);
    }

    _onReading(state) {
        const text = this._State.readoutText(state);
        for (const [view, w] of Object.entries(this._views))
            w.canvas.setReadout(view === state.hoverView ? text : '');
        this._describeViews(state);
    }

    _describeViews(state) {
        const text = this._State.readoutText(state);
        for (const [view, w] of Object.entries(this._views)) {
            const plan = state.plans?.[view];
            if (plan)
                w.canvas.describe(`${plan.title} view`, this._State.viewDescription(state, view, view === state.hoverView ? text : ''));
        }
    }

    /** V: the next view becomes the one Page Up/Down act on. */
    _cycleView() {
        const order = ['coronal', 'sagittal', 'axial'].filter(v => this._views[v]);
        if (!order.length)
            return false;
        this._hovered = order[(order.indexOf(this._hovered) + 1) % order.length];
        this._keyboardView = true;
        this._showActive();
        this._debug(`active view ${this._hovered}`);
        return true;
    }

    _showActive() {
        if (this._keyboardView) {
            for (const [view, w] of Object.entries(this._views))
                w.setActive(view === this._hovered);
        }
    }

    _fillMetadata(state, pending = false) {
        if (!pending && this._panelTimer) {
            GLib.source_remove(this._panelTimer);
            this._panelTimer = 0;
        }
        if (!this._metaBox)
            return;
        if (!this._headerShown) {
            this._headerShown = true;
            this._debug(`metadata shown${state.stats ? ' with statistics' : ' (statistics pending)'}`);
        }
        const State = this._State;
        let child;
        while ((child = this._metaBox.get_first_child()))
            this._metaBox.remove(child);
        const h = state.header;
        this._metaBox.append(label(h.name, ['np-meta-title'], {xalign: 0, wrap: true, wrap_mode: 2 /* WORD_CHAR */,
            max_width_chars: 40, selectable: true, tooltip_text: h.path}));
        for (const w of (this._error ? [this._error] : []).concat(h.warnings, state.stats?.notes ?? []))
            this._metaBox.append(label(w, ['np-warning'], {xalign: 0, wrap: true}));
        // Four sections, two by two: Geometry | Data, File | Affine.
        const sections = State.metadataSections(state, {pending}).filter(sec => sec.text === undefined || this._settings.affine);
        this._metaBox.append(this._sectionsGrid(sections));
        // Segmentations: every label with its colour, name and volume.
        const rows = State.labelRows(state);
        if (rows.length && (this._settings.panelRows ?? ['label-list']).includes('label-list')) {
            const more = state.labelList.complete ? '' : '+';
            const grid = new Gtk.Grid({column_spacing: 8, row_spacing: 1, margin_top: 4});
            rows.forEach((r, i) => {
                grid.attach(label(r.swatch, [], {use_markup: true}), 0, i, 1, 1);
                grid.attach(label(r.name, [], {use_markup: true, xalign: 0, selectable: true, hexpand: true, tooltip_text: `label ${r.value}`}), 1, i, 1, 1);
                grid.attach(label(r.volume, ['np-key'], {xalign: 1}), 2, i, 1, 1);
            });
            const expander = new Gtk.Expander({label: `Labels · ${rows.length}${more}`, expanded: !!this._labelsOpen,
                margin_top: 10, child: grid});
            this._track(expander, expander.connect('notify::expanded', e => {
                this._labelsOpen = e.expanded;
            }));
            this._metaBox.append(expander);
        }
    }

    _buildToolbar() {
        // Same look as Sushi's own toolbars (pdf.blp): a pill-shaped OSD at the bottom.
        const box = new Gtk.Box({spacing: 6, css_classes: ['osd', 'pill-osd'], halign: Gtk.Align.CENTER,
            valign: Gtk.Align.END});
        const make = (name, text, tooltip, callback) => {
            const b = new Gtk.Button({label: text, tooltip_text: tooltip, css_classes: ['flat', 'pill']});
            this._track(b, b.connect('clicked', callback));
            box.append(b);
            this._buttons[name] = b;
        };
        make('mode', 'Neurological', 'Display convention (O)', () => this._cycleMode());
        make('colormap', 'Labels', 'Colours / grayscale (L)', () => this._toggleColormap());
        make('range', 'Full width', 'Shift range shown (Z)', () => this._toggleRange());
        const center = new Gtk.Button({icon_name: 'find-location-symbolic', tooltip_text: 'Recenter (C)',
            css_classes: ['flat', 'circular']});
        this._buttons.center = center;
        this._track(center, center.connect('clicked', () => this._state?.recenter()));
        box.append(center);
        const prefs = new Gtk.Button({icon_name: 'emblem-system-symbolic', tooltip_text: 'Preferences of GNOME NIfTI Preview',
            css_classes: ['flat', 'circular']});
        this._buttons.preferences = prefs;
        this._track(prefs, prefs.connect('clicked', () => {
            try {
                Gio.Subprocess.new(['gjs', GLib.build_filenamev([SHARED_DIR, 'niftiPreview', 'preferences.js'])], Gio.SubprocessFlags.NONE);
            } catch (e) {
                logError(e, 'nifti-preview: preferences');
            }
        }));
        box.append(prefs);
        this._updateButtons();
        return box;
    }

    _updateButtons() {
        const s = this._state;
        const names = {neurological: 'Neurological', radiological: 'Radiological', stored: 'Stored axes'};
        this._buttons.mode?.set_label(names[s?.mode ?? 'neurological']);
        this._buttons.mode?.set_sensitive(!!s?.header?.orientation_known);
        const mrs = s?.header?.mrs;
        if (mrs) {
            this._buttons.colormap?.set_visible(!!this._plot?.spectrum);
            this._buttons.colormap?.set_label(this._plot?.part === 'magnitude' ? 'Magnitude' : 'Real part');
            this._buttons.colormap?.set_tooltip_text('Real part / magnitude (L)');
        } else {
            this._buttons.colormap?.set_visible(!!this._State?.nextColormap(s));
            this._buttons.colormap?.set_label(this._State?.COLORMAP_NAMES[s?.effectiveColormap] ?? 'Grayscale');
        }
        this._buttons.range?.set_visible(!!this._plot?.hasRange());
        this._buttons.range?.set_label(this._plot?.rangeLabel() ?? '');
        // A single-voxel spectrum has no views to orient or recenter.
        this._buttons.mode?.set_visible(!mrs?.single_voxel);
        this._buttons.center?.set_visible(!mrs?.single_voxel);
    }

    _cycleMode() {
        const s = this._state;
        if (!s?.header?.orientation_known)
            return;
        s.setDisplay({mode: s.nextMode()}).then(() => this._refresh(s)).catch(e => this._onError(e));
    }

    _toggleColormap() {
        if (this._plot) {
            this._togglePart();
            return;
        }
        const s = this._state;
        const next = this._State?.nextColormap(s);
        if (!next)
            return;
        s.setDisplay({colormap: next})
            .then(() => this._refresh(s)).catch(e => this._onError(e));
    }

    _hookKeys() {
        const root = this.get_root();
        if (!root || this._keyController)
            return;
        // Capture phase, before Sushi's shortcuts; unhandled keys keep their meaning.
        this._keyController = new Gtk.EventControllerKey({propagation_phase: Gtk.PropagationPhase.CAPTURE});
        this._track(this._keyController, this._keyController.connect('key-pressed', (c, keyval, keycode, mods) => {
            if (mods & (Gdk.ModifierType.CONTROL_MASK | Gdk.ModifierType.ALT_MASK) || this._about)
                return false;
            // Page Up/Down, Home and End act in the side panel when it has the focus.
            const focus = root.get_focus();
            if (focus && this._scroller && (focus === this._scroller || focus.is_ancestor(this._scroller)) &&
                [Gdk.KEY_Page_Up, Gdk.KEY_Page_Down, Gdk.KEY_Home, Gdk.KEY_End].includes(keyval))
                return false;
            const browsable = !!this._state?.browsable();
            switch (Gdk.keyval_to_lower(keyval)) {
            case Gdk.KEY_Page_Up:
                this._state?.scroll(this._hovered, 1);
                return true;
            case Gdk.KEY_Page_Down:
                this._state?.scroll(this._hovered, -1);
                return true;
            case Gdk.KEY_o:
                this._cycleMode();
                return true;
            case Gdk.KEY_l:
                this._toggleColormap();
                return true;
            case Gdk.KEY_z:
                if (!this._plot)
                    return false;
                this._toggleRange();
                return true;
            case Gdk.KEY_c:
                this._state?.recenter();
                return true;
            case Gdk.KEY_v:
                return this._cycleView();
            // 4D: previous / next volume, first / last.
            case Gdk.KEY_comma:
            case Gdk.KEY_period:
                if (!browsable)
                    return false;
                this._state.stepVolume(keyval === Gdk.KEY_comma ? -1 : 1);
                return true;
            case Gdk.KEY_Home:
            case Gdk.KEY_End:
                if (!browsable)
                    return false;
                this._state.setVolume(keyval === Gdk.KEY_Home ? 0 : this._state.header.n_volumes - 1);
                return true;
            // DICOM: previous / next series.
            case Gdk.KEY_bracketleft:
            case Gdk.KEY_bracketright:
                return this._stepSeries(keyval === Gdk.KEY_bracketleft ? -1 : 1);
            default:
                return false;
            }
        }));
        root.add_controller(this._keyController);
        this._keyRoot = root;
    }

    cleanup() {
        for (const id of [this._startIdle, this._panelTimer]) {
            if (id)
                GLib.source_remove(id);
        }
        this._startIdle = this._panelTimer = 0;
        if (this._keyController && this._keyRoot) {
            this._keyRoot.remove_controller(this._keyController);
            this._keyController = null;
            this._keyRoot = null;
        }
        this._state?.destroy();
        this._state = null;
        this._client?.close();
        this._client = null;
        // Sushi requires plugins not to keep themselves alive after unloading:
        // disconnect every handler (their closures reference this renderer).
        for (const [object, id] of this._handlers ?? []) {
            try {
                object.disconnect(id);
            } catch (e) {}
        }
        this._handlers = [];
        // Sushi 51.0 keeps unloaded renderer objects alive (observed even with a
        // minimal plugin), so drop the widget tree and textures explicitly.
        for (const v of Object.values(this._views ?? {}))
            v.canvas.release();
        this._views = {};
        this._plotArea?.set_draw_func(null);
        this._timecourseArea?.set_draw_func(null);
        this._plot = this._plotArea = this._grid = null;
        this._timecourse = this._timecourseArea = this._volumeScale = this._volumeLabel = null;
        this._metaBox = null;
        this._scroller = null;
        this._side = this._seriesRow = this._seriesDrop = null;
        this._spinner = null;
        this._overlay = null;
        this.set_child(null);
    }
};

export const contentTypes = ['image/x-nifti', 'image/x-compressed-nifti', 'image/x-mgh', 'image/x-compressed-mgh', 'image/x-mrtrix',
    'image/x-compressed-mrtrix', 'image/x-nrrd', 'image/x-nrrd-header', 'image/x-metaimage', 'image/x-metaimage-header',
    'application/dicom'];
