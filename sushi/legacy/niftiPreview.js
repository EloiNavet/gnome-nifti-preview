// SPDX-License-Identifier: GPL-2.0-or-later
// GNOME NIfTI Preview — NIfTI previewer for GNOME Sushi 46–50 (GTK 3, legacy GJS imports).
//
// Installed as ~/.local/share/sushi/viewers/niftiPreview.js. Sushi imports every
// file in that directory when it starts and does not guard against errors, so
// this module must never throw at import time: everything that can fail is
// done lazily inside the renderer, and a failure to define the class only
// disables this viewer (empty mimeTypes) instead of breaking all previews.

const {Gdk, GdkPixbuf, GLib, GObject, Gio, Gtk} = imports.gi;
const Cairo = imports.cairo;

var mimeTypes = [];
var Klass = null;

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
.np-message { color: #fff; background-color: rgba(0, 0, 0, 0.75); border-radius: 8px; padding: 12px 18px; }
.np-meta { padding: 14px 16px; }
.np-meta-title { font-weight: bold; font-size: larger; }
.np-section { font-weight: bold; font-size: smaller; margin-top: 10px; opacity: 0.6; }
.np-key { opacity: 0.6; }
.np-mono { font-family: monospace; font-size: smaller; }
.np-warning { font-size: smaller; }
`;

let _cssInstalled = false;
function installCss() {
    if (_cssInstalled)
        return;
    const provider = new Gtk.CssProvider();
    provider.load_from_data(CSS);
    Gtk.StyleContext.add_provider_for_screen(Gdk.Screen.get_default(), provider,
        Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION);
    _cssInstalled = true;
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

/** Lower-case letter keys, so that Shift and Caps Lock do not matter. */
function lowerKey(keyval) {
    return Gdk.keyval_to_lower(keyval);
}

function label(text, classes = [], props = {}) {
    const l = new Gtk.Label({label: text, ...props});
    for (const c of classes)
        l.get_style_context().add_class(c);
    return l;
}

function defineSliceView() {
    return GObject.registerClass({
        Signals: {
            'scroll-slice': {param_types: [GObject.TYPE_INT]},
            'pick': {param_types: [GObject.TYPE_DOUBLE, GObject.TYPE_DOUBLE]},
            // Image pixel under the pointer, (-1, -1) when it leaves the image.
            'hover': {param_types: [GObject.TYPE_DOUBLE, GObject.TYPE_DOUBLE]},
            'hovered': {},
        },
    }, class NiftiSliceView extends Gtk.Overlay {
        _init(view) {
            super._init({hexpand: true, vexpand: true});
            this.view = view;
            this.get_style_context().add_class('np-view');
            this._pixbuf = null;
            this._surface = null;
            this._plan = null;
            this._cross = null;
            this._nearest = false;
            this._scrollAccum = 0;
            this._readout = '';
            this._pointer = null;

            this._area = new Gtk.DrawingArea({hexpand: true, vexpand: true, can_focus: false});
            this._area.add_events(Gdk.EventMask.BUTTON_PRESS_MASK | Gdk.EventMask.BUTTON_RELEASE_MASK |
                Gdk.EventMask.POINTER_MOTION_MASK | Gdk.EventMask.SCROLL_MASK |
                Gdk.EventMask.SMOOTH_SCROLL_MASK | Gdk.EventMask.ENTER_NOTIFY_MASK |
                Gdk.EventMask.LEAVE_NOTIFY_MASK);
            this._area.connect('draw', (w, cr) => this._draw(w, cr));
            this._area.connect('scroll-event', (w, ev) => this._onScroll(ev));
            this._area.connect('button-press-event', (w, ev) => this._onButton(ev));
            this._area.connect('motion-notify-event', (w, ev) => this._onMotion(ev));
            this._area.connect('enter-notify-event', () => {
                this.emit('hovered');
                return false;
            });
            this._area.connect('leave-notify-event', () => {
                this._pointer = null;
                this.emit('hover', -1, -1);
                return false;
            });
            this.add(this._area);
            try {
                this._area.get_accessible()?.set_role(imports.gi.Atk.Role.IMAGE);
            } catch (e) {}

            const edge = (h, v) => label('', ['np-edge'], {halign: h, valign: v, no_show_all: false});
            this._labels = {
                left: edge(Gtk.Align.START, Gtk.Align.CENTER),
                right: edge(Gtk.Align.END, Gtk.Align.CENTER),
                top: edge(Gtk.Align.CENTER, Gtk.Align.START),
                bottom: edge(Gtk.Align.CENTER, Gtk.Align.END),
            };
            this._title = label('', ['np-title'], {halign: Gtk.Align.START, valign: Gtk.Align.START});
            this._note = label('', ['np-title'], {halign: Gtk.Align.CENTER, valign: Gtk.Align.CENTER,
                no_show_all: true});
            for (const l of [...Object.values(this._labels), this._title, this._note]) {
                this.add_overlay(l);
                this.set_overlay_pass_through(l, true);
            }
        }

        setPlan(plan, index) {
            this._plan = plan;
            for (const side of ['left', 'right', 'top', 'bottom'])
                this._labels[side].set_label(plan.labels[side]);
            this.setIndex(index);
        }

        setIndex(index) {
            if (this._plan)
                this._title.set_label(`${this._plan.title}  ${index} / ${this._plan.n_slices - 1}`);
        }

        /** Accessible name and description (screen readers). */
        describe(name, description) {
            const a = this._area.get_accessible();
            if (!a)
                return;
            if (a.get_name() !== name)
                a.set_name(name);
            if (a.get_description() !== description)
                a.set_description(description);
        }

        /** The view Page Up/Down act on, once chosen with the keyboard (V). */
        setActive(active) {
            const style = this._title.get_style_context();
            if (active)
                style.add_class('np-active');
            else
                style.remove_class('np-active');
        }

        setImage(reply, nearest) {
            const bytes = new GLib.Bytes(reply.pixels);
            this._pixbuf = GdkPixbuf.Pixbuf.new_from_bytes(bytes, GdkPixbuf.Colorspace.RGB, false, 8,
                reply.width, reply.height, reply.width * 3);
            // Converted once here, not on every redraw (each pointer move redraws).
            this._surface = this._pixbuf ? Gdk.cairo_surface_create_from_pixbuf(this._pixbuf, 1, null) : null;
            this._nearest = nearest;
            this._area.queue_draw();
        }

        showNote(text) {
            this._note.set_label(text);
            this._note.show();
        }

        setCrosshair(cross) {
            this._cross = cross;
            this._area.queue_draw();
        }

        /** Text drawn next to the pointer ('' hides it). */
        setReadout(text) {
            if (text === this._readout)
                return;
            this._readout = text;
            this._area.queue_draw();
        }

        _drawReadout(cr, w, h) {
            if (!this._readout || !this._pointer)
                return;
            const layout = this._area.create_pango_layout(this._readout);
            const [, logical] = layout.get_pixel_extents();
            const pad = 5;
            const bw = logical.width + 2 * pad;
            const bh = logical.height + 2 * pad - 2;
            let [x, y] = this._pointer;
            x = x + 14 + bw > w ? x - 8 - bw : x + 14;
            y = y + 18 + bh > h ? y - 8 - bh : y + 18;
            x = Math.round(Math.max(2, x));
            y = Math.round(Math.max(2, y));
            const r = 5;
            cr.newSubPath();
            cr.arc(x + bw - r, y + r, r, -Math.PI / 2, 0);
            cr.arc(x + bw - r, y + bh - r, r, 0, Math.PI / 2);
            cr.arc(x + r, y + bh - r, r, Math.PI / 2, Math.PI);
            cr.arc(x + r, y + r, r, Math.PI, 1.5 * Math.PI);
            cr.closePath();
            cr.setSourceRGBA(0, 0, 0, 0.75);
            cr.fill();
            cr.setSourceRGBA(1, 1, 1, 0.95);
            cr.moveTo(x + pad, y + pad - 1);
            imports.gi.PangoCairo.show_layout(cr, layout);
        }

        _geometry(w, h) {
            return sliceGeometry(this._plan, w, h);
        }

        _draw(widget, cr) {
            try {
                this._paint(widget, cr);
            } catch (e) {
                logError(e, 'nifti-preview: draw');
            } finally {
                cr.$dispose();
            }
            return false;
        }

        _paint(widget, cr) {
            const w = widget.get_allocated_width();
            const h = widget.get_allocated_height();
            cr.setSourceRGB(0, 0, 0);
            cr.paint();
            const g = this._surface && this._plan ? this._geometry(w, h) : null;
            if (g) {
                const [ox, oy, sx, sy] = g;
                cr.save();
                try {
                    cr.translate(ox, oy);
                    cr.scale(sx, sy);
                    cr.setSourceSurface(this._surface, 0, 0);
                    // Labels never get interpolated colours; intensities are smoothed,
                    // unless a voxel is large on screen (then it stays sharp).
                    const sharp = this._nearest || Math.min(sx, sy) >= SHARP_FROM;
                    cr.getSource().setFilter(sharp ? Cairo.Filter.NEAREST : Cairo.Filter.GOOD);
                    cr.paint();
                } finally {
                    cr.restore();
                }
                if (this._cross) {
                    const x = Math.round(ox + (this._cross[0] + 0.5) * sx) + 0.5;
                    const y = Math.round(oy + (this._cross[1] + 0.5) * sy) + 0.5;
                    cr.setSourceRGBA(0.38, 0.68, 1.0, 0.7);
                    cr.setLineWidth(1);
                    cr.moveTo(x, oy);
                    cr.lineTo(x, oy + this._plan.height * sy);
                    cr.moveTo(ox, y);
                    cr.lineTo(ox + this._plan.width * sx, y);
                    cr.stroke();
                }
                this._drawReadout(cr, w, h);
            }
        }

        _onScroll(event) {
            // X11 sends both a smooth event and an emulated discrete one per notch.
            if (event.get_pointer_emulated())
                return true;
            const [hasDir, direction] = event.get_scroll_direction();
            let steps = 0;
            if (hasDir && direction === Gdk.ScrollDirection.UP)
                steps = 1;
            else if (hasDir && direction === Gdk.ScrollDirection.DOWN)
                steps = -1;
            else {
                const [ok, , dy] = event.get_scroll_deltas();
                if (ok) {
                    this._scrollAccum -= dy;
                    steps = Math.trunc(this._scrollAccum);
                    this._scrollAccum -= steps;
                }
            }
            if (steps)
                this.emit('scroll-slice', steps);
            return true;
        }

        _imagePixel(x, y) {
            if (!this._plan || !this._surface)
                return null;
            const g = this._geometry(this._area.get_allocated_width(), this._area.get_allocated_height());
            if (!g)
                return null;
            const [ox, oy, sx, sy] = g;
            const col = (x - ox) / sx;
            const row = (y - oy) / sy;
            return col >= 0 && row >= 0 && col < this._plan.width && row < this._plan.height ? [col, row] : null;
        }

        _onMotion(event) {
            const [, x, y] = event.get_coords();
            const px = this._imagePixel(x, y);
            this._pointer = px ? [x, y] : null;
            this.emit('hover', px ? px[0] : -1, px ? px[1] : -1);
            if (this._readout)
                this._area.queue_draw();
            return this._onButton(event);
        }

        _onButton(event) {
            const [, state] = event.get_state();
            const [, button] = event.get_button();
            const pressed = event.get_event_type() === Gdk.EventType.BUTTON_PRESS ? button === 1
                : (state & Gdk.ModifierType.BUTTON1_MASK) !== 0;
            if (!pressed)
                return false;
            const [, x, y] = event.get_coords();
            const px = this._imagePixel(x, y);
            if (px)
                this.emit('pick', px[0], px[1]);
            return true;
        }
    });
}

function defineRenderer(Renderer, SliceView) {
    return GObject.registerClass({
        Implements: [Renderer.Renderer],
        Properties: {
            fullscreen: GObject.ParamSpec.boolean('fullscreen', '', '', GObject.ParamFlags.READABLE, false),
            ready: GObject.ParamSpec.boolean('ready', '', '', GObject.ParamFlags.READABLE, false),
        },
    }, class NiftiPreviewRenderer extends Gtk.Overlay {
        get ready() {
            return !!this._ready;
        }

        get fullscreen() {
            return !!this._fullscreen;
        }

        get resizePolicy() {
            return Renderer.ResizePolicy.MAX_SIZE;
        }

        get moveOnClick() {
            return false;
        }

        _init(file, fileInfo) {
            super._init({hexpand: true, vexpand: true});
            installCss();
            this._file = file;
            this._t0 = GLib.get_monotonic_time();
            this._firstSlices = new Set();
            this._state = null;
            this._client = null;
            this._destroyed = false;
            this._hovered = 'axial';
            this._keyboardView = false; // V pressed: the active view is shown
            this._keyHandler = 0;
            this._buttons = {};

            this._views = {};
            const grid = new Gtk.Grid({row_homogeneous: true, column_homogeneous: true,
                row_spacing: 1, column_spacing: 1, hexpand: true, vexpand: true});
            this._grid = grid;
            this._plot = null; //     spectroscopy: SpectrumPlot and its drawing area
            this._plotArea = null;
            const positions = {coronal: [0, 0], sagittal: [1, 0], axial: [0, 1]};
            for (const [view, [x, y]] of Object.entries(positions)) {
                const v = new SliceView(view);
                v.connect('scroll-slice', (w, steps) => this._state?.scroll(view, steps));
                v.connect('pick', (w, col, row) => this._pick(view, col, row));
                v.connect('hover', (w, col, row) => {
                    if (this._settings?.hover)
                        this._state?.hover(view, col < 0 ? null : col, row);
                });
                v.connect('hovered', () => {
                    this._hovered = view;
                    this._showActive();
                });
                grid.attach(v, x, y, 1, 1);
                this._views[view] = v;
            }
            this._meta = new Gtk.ScrolledWindow({hscrollbar_policy: Gtk.PolicyType.NEVER, hexpand: true, vexpand: true});
            this._metaBox = new Gtk.Box({orientation: Gtk.Orientation.VERTICAL, spacing: 2});
            this._metaBox.get_style_context().add_class('np-meta');
            this._meta.add(this._metaBox);
            grid.attach(this._meta, 1, 1, 1, 1);
            this.add(grid);

            this._spinner = new Gtk.Spinner({active: true, width_request: 32, height_request: 32,
                halign: Gtk.Align.CENTER, valign: Gtk.Align.CENTER});
            this.add_overlay(this._spinner);
            this._error = null;

            this.connect('destroy', () => this._onDestroy());
            this.connect('realize', () => this._hookKeys());

            // Show the window right away (with a spinner) while the backend loads.
            this._ready = true;
            this.notify('ready');
            // Sushi 46-50 can destroy this renderer before the idle runs (fast
            // arrow keys, or closing at once): the idle is then removed, so no
            // backend is ever started for it.
            this._startIdle = GLib.idle_add(GLib.PRIORITY_DEFAULT, () => {
                this._startIdle = 0;
                if (!this._destroyed)
                    this._start().catch(e => this._fail(e));
                return GLib.SOURCE_REMOVE;
            });
        }

        async _start() {
            if (this._destroyed)
                return;
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
                onStats: s => this._onStats(s),
                onSlice: (view, reply, s) => this._onSlice(view, reply, s),
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
                if (this._destroyed || e.code === 'closed')
                    return;
                if (this._state.header) {
                    // The header is valid: keep the metadata, explain the data problem.
                    this._showMessage(e.message);
                    return;
                }
                throw e;
            }
        }

        _fail(error) {
            if (this._destroyed || error?.code === 'closed')
                return;
            // Invalid files are expected: only log unexpected exceptions as JS errors.
            if (error?.name === 'BackendError')
                printerr(`nifti-preview: ${error.code}: ${error.message}`);
            else
                logError(error, 'nifti-preview');
            const gerror = new GLib.Error(Gio.IOErrorEnum, Gio.IOErrorEnum.FAILED,
                `Cannot preview this file: ${error.message}`);
            this.emit('error', gerror);
        }

        _onError(error) {
            if (!this._destroyed && error?.code !== 'closed')
                this._showMessage(error.message);
        }

        /** Data could not be shown: say so in the views, details in the side panel. */
        _showMessage(text) {
            if (this._destroyed)
                return;
            this._spinner.hide();
            this._error = text;
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
         * takes the top row and the panel the bottom row. Spectroscopic
         * imaging keeps its maps; the plot of the cursor voxel sits above the
         * panel.
         */
        _setupSpectroscopy(state) {
            if (this._plot || !state.header?.mrs)
                return;
            this._plot = new this._Plot.SpectrumPlot();
            const area = new Gtk.DrawingArea({hexpand: true, vexpand: true, can_focus: false});
            try {
                area.get_accessible()?.set_role(imports.gi.Atk.Role.IMAGE);
            } catch (e) {}
            area.get_style_context().add_class('np-view');
            area.add_events(Gdk.EventMask.POINTER_MOTION_MASK | Gdk.EventMask.LEAVE_NOTIFY_MASK);
            area.connect('draw', (w, cr) => {
                try {
                    this._plot.draw(cr, w.get_allocated_width(), w.get_allocated_height(), t => w.create_pango_layout(t));
                } catch (e) {
                    logError(e, 'nifti-preview: spectrum');
                } finally {
                    cr.$dispose();
                }
                return false;
            });
            area.connect('motion-notify-event', (w, event) => {
                this._plot.pointer = event.get_coords()[1];
                w.queue_draw();
                return false;
            });
            area.connect('leave-notify-event', w => {
                this._plot.pointer = null;
                w.queue_draw();
                return false;
            });
            this._plotArea = area;
            this._grid.remove(this._meta);
            if (state.header.mrs.single_voxel) {
                for (const v of Object.values(this._views))
                    v.destroy();
                this._views = {};
                this._grid.attach(area, 0, 0, 2, 1);
                this._grid.attach(this._meta, 0, 1, 2, 1);
            } else {
                const box = new Gtk.Box({orientation: Gtk.Orientation.VERTICAL, spacing: 1, homogeneous: true});
                box.pack_start(area, true, true, 0);
                box.pack_start(this._meta, true, true, 0);
                this._grid.attach(box, 1, 1, 1, 1);
            }
            this._grid.show_all();
            this._updatePlotTitle(state);
        }

        _updatePlotTitle(state) {
            if (!this._plot)
                return;
            const m = state.header.mrs;
            const what = this._plot.part === 'magnitude' ? 'magnitude' : 'real part';
            const voxel = state.spectrum?.voxel;
            const combined = m.combined ?? m.transients;
            this._plot.title = m.single_voxel
                ? `${m.nucleus ?? ''} spectrum · ${combined > 1 ? `${combined} transients combined · ` : ''}${what}`.trim()
                : `Voxel ${voxel ? voxel.join(', ') : '…'} · ${what}`;
            const a = this._plotArea?.get_accessible();
            if (a) {
                a.set_name('Spectrum');
                a.set_description(`${this._plot.title}; ${this._plot.rangeLabel()}${this._plot.message ? `; ${this._plot.message}` : ''}`);
            }
        }

        _onSpectrum(state) {
            if (this._destroyed || !this._plot)
                return;
            if (state.header.mrs.single_voxel) {
                this._spinner.hide();
                this._spinner.active = false;
            }
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

        _debug(what) {
            if (DEBUG)
                printerr(`nifti-preview: ${what} after ${((GLib.get_monotonic_time() - this._t0) / 1000).toFixed(1)} ms`);
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
                    if (!this._destroyed && this._state?.header && !this._state.stats && !this._error)
                        this._fillMetadata(this._state, true);
                    return GLib.SOURCE_REMOVE;
                });
            this._updateButtons();
        }

        _onStats(state) {
            this._debug('statistics received');
            this._fillMetadata(state);
            this._updateButtons();
        }

        _onSlice(view, reply, state) {
            const plan = state.plan(view);
            if (this._destroyed || !this._views[view] || reply.view !== view ||
                reply.width !== plan.width || reply.height !== plan.height)
                return;
            this._spinner.hide();
            this._spinner.active = false;
            this._views[view].setImage(reply, reply.colormap === 'labels');
            this._views[view].setIndex(reply.index);
            if (DEBUG && this._firstSlices.size < 3 && !this._firstSlices.has(view)) {
                this._firstSlices.add(view);
                if (this._firstSlices.size === 3)
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
                w.setCrosshair(this._crossVisible ? state.crosshair(view) : null);
            }
            this._describeViews(state);
        }

        _onReading(state) {
            const text = this._State.readoutText(state);
            for (const [view, w] of Object.entries(this._views))
                w.setReadout(view === state.hoverView ? text : '');
            this._describeViews(state);
        }

        _describeViews(state) {
            const text = this._State.readoutText(state);
            for (const [view, w] of Object.entries(this._views)) {
                const plan = state.plans?.[view];
                if (plan)
                    w.describe(`${plan.title} view`, this._State.viewDescription(state, view, view === state.hoverView ? text : ''));
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
            if (!this._headerShown) {
                this._headerShown = true;
                this._debug(`metadata shown${state.stats ? ' with statistics' : ' (statistics pending)'}`);
            }
            const State = this._State;
            for (const child of this._metaBox.get_children())
                child.destroy();
            const h = state.header;
            const title = label(h.name, ['np-meta-title'], {xalign: 0, wrap: true, wrap_mode: 2 /* WORD_CHAR */,
                max_width_chars: 40, selectable: true});
            title.set_tooltip_text(h.path);
            this._metaBox.pack_start(title, false, false, 0);
            for (const w of (this._error ? [this._error] : []).concat(h.warnings, state.stats?.notes ?? []))
                this._metaBox.pack_start(label(w, ['np-warning'], {xalign: 0, wrap: true}), false, false, 0);
            // Four sections, two by two: Geometry | Data, File | Affine.
            const sections = new Gtk.Grid({column_spacing: 28, row_spacing: 4, column_homogeneous: true});
            State.metadataSections(state, {pending}).filter(sec => sec.text === undefined || this._settings.affine).forEach((sec, i) => {
                const box = new Gtk.Box({orientation: Gtk.Orientation.VERTICAL, spacing: 3});
                box.pack_start(label(sec.title.toUpperCase(), ['np-section'], {xalign: 0}), false, false, 0);
                if (sec.text !== undefined) {
                    box.pack_start(label(sec.text, ['np-mono'], {xalign: 0, selectable: true}), false, false, 0);
                } else {
                    const rows = new Gtk.Grid({column_spacing: 10, row_spacing: 2});
                    sec.rows.forEach(([key, value], row) => {
                        rows.attach(label(key, ['np-key'], {xalign: 0, valign: Gtk.Align.START}), 0, row, 1, 1);
                        rows.attach(label(value, [], {xalign: 0, selectable: true, wrap: true, wrap_mode: 2 /* WORD_CHAR */,
                            max_width_chars: 22}), 1, row, 1, 1);
                    });
                    box.pack_start(rows, false, false, 0);
                }
                sections.attach(box, i % 2, Math.floor(i / 2), 1, 1);
            });
            this._metaBox.pack_start(sections, false, false, 0);
            this._metaBox.show_all();
        }

        // ------------------------------------------------------------ toolbar

        populateToolbar(box) {
            const make = (name, text, tooltip, callback) => {
                const b = new Gtk.Button({label: text, relief: Gtk.ReliefStyle.NONE, tooltip_text: tooltip});
                b.connect('clicked', () => {
                    this.toolbar?.resetTimeout();
                    callback();
                });
                box.add(b);
                this._buttons[name] = b;
                return b;
            };
            make('mode', 'Neurological', 'Display convention (O)', () => this._cycleMode());
            make('colormap', 'Labels', 'Colours / grayscale (L)', () => this._toggleColormap());
            make('range', 'Full width', 'Shift range shown (Z)', () => this._toggleRange());
            const center = Gtk.Button.new_from_icon_name('find-location-symbolic', Gtk.IconSize.MENU);
            this._buttons.center = center;
            center.set_relief(Gtk.ReliefStyle.NONE);
            center.set_tooltip_text('Recenter (C)');
            center.connect('clicked', () => {
                this.toolbar?.resetTimeout();
                this._state?.recenter();
            });
            box.add(center);
            // Sushi appends its fullscreen button after this method returns and adds the
            // toolbar as an overlay after show_all(): show everything once that is done.
            this._toolbarIdle = GLib.idle_add(GLib.PRIORITY_DEFAULT, () => {
                this._toolbarIdle = 0;
                if (this._destroyed)
                    return GLib.SOURCE_REMOVE;
                box.show_all();
                box.get_parent()?.show();
                this._updateButtons();
                return GLib.SOURCE_REMOVE;
            });
            this._updateButtons();
        }

        _updateButtons() {
            const s = this._state;
            const names = {neurological: 'Neurological', radiological: 'Radiological', stored: 'Stored axes'};
            if (this._buttons.mode) {
                this._buttons.mode.set_label(names[s?.mode ?? 'neurological']);
                this._buttons.mode.set_sensitive(!!s?.header?.orientation_known);
            }
            const mrs = s?.header?.mrs;
            if (this._buttons.colormap) {
                if (mrs) {
                    this._buttons.colormap.set_visible(!!this._plot?.spectrum);
                    this._buttons.colormap.set_label(this._plot?.part === 'magnitude' ? 'Magnitude' : 'Real part');
                    this._buttons.colormap.set_tooltip_text('Real part / magnitude (L)');
                } else {
                    this._buttons.colormap.set_visible(!!this._State?.nextColormap(s));
                    this._buttons.colormap.set_label(this._State?.COLORMAP_NAMES[s?.effectiveColormap] ?? 'Grayscale');
                }
            }
            if (this._buttons.range) {
                this._buttons.range.set_visible(!!this._plot?.hasRange());
                this._buttons.range.set_label(this._plot?.rangeLabel() ?? '');
            }
            // A single-voxel spectrum has no views to orient or recenter.
            this._buttons.mode?.set_visible(!mrs?.single_voxel);
            this._buttons.center?.set_visible(!mrs?.single_voxel);
        }

        _cycleMode() {
            const s = this._state;
            if (!s?.header?.orientation_known)
                return;
            s.setDisplay({mode: s.nextMode()}).then(() => {
                this._fillMetadata(s);
                this._updateButtons();
            }).catch(e => this._onError(e));
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
            s.setDisplay({colormap: next}).then(() => {
                this._fillMetadata(s);
                this._updateButtons();
            }).catch(e => this._onError(e));
        }

        // ----------------------------------------------------------- keyboard

        _hookKeys() {
            const top = this.get_toplevel();
            if (!(top instanceof Gtk.Window) || this._keyHandler)
                return;
            this._keyWindow = top;
            // Runs before the window's accelerators; unhandled keys (Space,
            // Escape, arrows...) keep their Sushi behaviour.
            this._keyHandler = top.connect('key-press-event', (w, event) => {
                const [, keyval] = event.get_keyval();
                const [, mods] = event.get_state();
                if (mods & (Gdk.ModifierType.CONTROL_MASK | Gdk.ModifierType.MOD1_MASK))
                    return false;
                // Page Up/Down scroll the side panel when it has the focus.
                const focus = w.get_focus();
                if (focus && (focus === this._meta || focus.is_ancestor(this._meta)) &&
                    (keyval === Gdk.KEY_Page_Up || keyval === Gdk.KEY_Page_Down))
                    return false;
                switch (lowerKey(keyval)) {
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
                    if (this._plot)
                        this._togglePart();
                    else
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
            });
        }

        _onDestroy() {
            this._destroyed = true;
            for (const id of [this._startIdle, this._toolbarIdle, this._panelTimer]) {
                if (id)
                    GLib.source_remove(id);
            }
            this._startIdle = this._toolbarIdle = this._panelTimer = 0;
            if (this._keyHandler && this._keyWindow) {
                this._keyWindow.disconnect(this._keyHandler);
                this._keyHandler = 0;
            }
            this._state?.destroy();
            this._state = null;
            this._client?.close();
            this._client = null;
        }
    });
}

try {
    const Renderer = imports.ui.renderer;
    Klass = defineRenderer(Renderer, defineSliceView());
    mimeTypes = ['image/x-nifti', 'image/x-compressed-nifti'];
} catch (e) {
    logError(e, 'nifti-preview: NIfTI viewer disabled');
}
