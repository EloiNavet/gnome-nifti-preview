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
            imports.niftiPreview.spectrumPlot];
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
                'pick': {param_types: [GObject.TYPE_DOUBLE, GObject.TYPE_DOUBLE]},
                // Image pixel under the pointer, (-1, -1) when it leaves the image.
                'hover': {param_types: [GObject.TYPE_DOUBLE, GObject.TYPE_DOUBLE]},
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

        const scroll = new Gtk.EventControllerScroll({flags: Gtk.EventControllerScrollFlags.VERTICAL});
        scroll.connect('scroll', (c, dx, dy) => {
            this._accum -= dy;
            const steps = Math.trunc(this._accum);
            this._accum -= steps;
            if (steps)
                this.emit('scroll-slice', steps);
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
        const sharp = this._nearest || Math.min(sx, sy) >= SHARP_FROM;
        snapshot.append_scaled_texture(this._texture, sharp ? Gsk.ScalingFilter.NEAREST : Gsk.ScalingFilter.LINEAR, rect);
        if (this._cross) {
            const blue = new Gdk.RGBA({red: 0.38, green: 0.68, blue: 1.0, alpha: 0.7});
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
            this._track(v.canvas, v.canvas.connect('pick', (w, col, row) => this._pick(view, col, row)));
            this._track(v.canvas, v.canvas.connect('hover', (w, col, row) => {
                if (this._settings?.hover)
                    this._state?.hover(view, col < 0 ? null : col, row);
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
        grid.attach(scroller, 1, 1, 1, 1);
        this._scroller = scroller;
        const overlay = new Gtk.Overlay({child: grid});
        this._spinner = new Gtk.Spinner({spinning: true, width_request: 32, height_request: 32,
            halign: Gtk.Align.CENTER, valign: Gtk.Align.CENTER, can_target: false});
        overlay.add_overlay(this._spinner);
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
            throw new Error('Only local files can be previewed.');
        const [Client, State, Settings, Plot] = loadShared();
        this._settings = Settings.load();
        this._State = State;
        this._Plot = Plot;
        try {
            this._client = new Client.BackendClient({env: Settings.backendEnv(this._settings)});
        } catch (e) {
            throw new Error(`The GNOME NIfTI Preview backend could not be started (${e.message}). Run install.sh again.`);
        }
        this._state = new State.ViewState(this._client, {
            onHeader: s => this._onHeader(s),
            onStats: s => {
                this._debug('statistics received');
                this._refresh(s);
            },
            onSlice: (view, reply) => this._onSlice(view, reply),
            onCursor: s => this._onCursor(s),
            onReading: s => this._onReading(s),
            onSpectrum: s => this._onSpectrum(s),
            onError: e => this._onError(e),
        }, {labelVolume: this._settings.labelVolume, hoverCoordinates: this._settings.hoverCoordinates});
        this._crossVisible = this._settings.crosshair === 'always';
        try {
            // Without label colours: grayscale, except the colours of RGB images.
            await this._state.open(path, this._settings.orientation, this._settings.labelColors ? 'auto' : 'rgb');
        } catch (e) {
            if (this.cancellable.is_cancelled() || e.code === 'closed')
                return;
            if (this._state.header) {
                this._showMessage(e.message);
                return;
            }
            throw e;
        }
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
        this._grid.remove(this._scroller);
        if (state.header.mrs.single_voxel) {
            for (const v of Object.values(this._views)) {
                v.canvas.release();
                this._grid.remove(v);
            }
            this._views = {};
            this._grid.attach(area, 0, 0, 2, 1);
            this._grid.attach(this._scroller, 0, 1, 2, 1);
        } else {
            const box = new Gtk.Box({orientation: Gtk.Orientation.VERTICAL, spacing: 1, homogeneous: true});
            box.append(area);
            box.append(this._scroller);
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

    _onHeader(state) {
        this._setupSpectroscopy(state);
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
        const sections = new Gtk.Grid({column_spacing: 28, row_spacing: 4, column_homogeneous: true});
        State.metadataSections(state, {pending}).filter(sec => sec.text === undefined || this._settings.affine).forEach((sec, i) => {
            const box = new Gtk.Box({orientation: Gtk.Orientation.VERTICAL, spacing: 3});
            box.append(label(sec.title.toUpperCase(), ['np-section'], {xalign: 0}));
            if (sec.text !== undefined) {
                box.append(label(sec.text, ['np-mono'], {xalign: 0, selectable: true}));
            } else {
                const rows = new Gtk.Grid({column_spacing: 10, row_spacing: 2});
                sec.rows.forEach(([key, value], row) => {
                    rows.attach(label(key, ['np-key'], {xalign: 0, valign: Gtk.Align.START}), 0, row, 1, 1);
                    rows.attach(label(value, [], {xalign: 0, selectable: true, wrap: true, wrap_mode: 2 /* WORD_CHAR */,
                        max_width_chars: 22}), 1, row, 1, 1);
                });
                box.append(rows);
            }
            sections.attach(box, i % 2, Math.floor(i / 2), 1, 1);
        });
        this._metaBox.append(sections);
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
            if (mods & (Gdk.ModifierType.CONTROL_MASK | Gdk.ModifierType.ALT_MASK))
                return false;
            // Page Up/Down scroll the side panel when it has the focus.
            const focus = root.get_focus();
            if (focus && this._scroller && (focus === this._scroller || focus.is_ancestor(this._scroller)) &&
                (keyval === Gdk.KEY_Page_Up || keyval === Gdk.KEY_Page_Down))
                return false;
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
        this._plot = this._plotArea = this._grid = null;
        this._metaBox = null;
        this._scroller = null;
        this._spinner = null;
        this.set_child(null);
    }
};

export const contentTypes = ['image/x-nifti', 'image/x-compressed-nifti'];
