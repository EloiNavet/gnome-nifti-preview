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
.np-volume { padding: 4px 12px 2px 12px; }
.np-volume-text { font-size: smaller; font-feature-settings: "tnum"; }
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

/** The Preferences window: a separate program (GTK 4), so that it outlives the preview. */
function openPreferences() {
    const script = GLib.build_filenamev([SHARED_DIR, 'niftiPreview', 'preferences.js']);
    try {
        Gio.Subprocess.new(['gjs', script], Gio.SubprocessFlags.NONE);
    } catch (e) {
        logError(e, 'nifti-preview: preferences');
    }
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
            // Shift + wheel: the volume of a 4D image.
            'scroll-volume': {param_types: [GObject.TYPE_INT]},
            'pick': {param_types: [GObject.TYPE_DOUBLE, GObject.TYPE_DOUBLE]},
            // Image pixel under the pointer, (-1, -1) when it leaves the image.
            'hover': {param_types: [GObject.TYPE_DOUBLE, GObject.TYPE_DOUBLE]},
            'hovered': {},
            // Right button: drag (dx, dy from where it was pressed) adjusts the
            // window; a double right-click resets it.
            'window-start': {},
            'window-drag': {param_types: [GObject.TYPE_DOUBLE, GObject.TYPE_DOUBLE]},
            'window-reset': {},
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
            this._dragFrom = null; // right-button press point
            this._interpolation = 'auto';
            this._crossColour = [0.38, 0.68, 1.0];

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

        /** Display settings: orientation letters, smoothing, cursor colour (#rrggbb). */
        configure({axisLabels = true, interpolation = 'auto', crosshairColor = '#61adff'} = {}) {
            for (const l of Object.values(this._labels)) {
                // show_all() would show them again otherwise.
                l.set_no_show_all(!axisLabels);
                l.set_visible(axisLabels);
            }
            this._interpolation = interpolation;
            const m = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(crosshairColor);
            if (m)
                this._crossColour = m.slice(1).map(h => parseInt(h, 16) / 255);
            this._area.queue_draw();
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

        hideNote() {
            this._note.hide();
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
                    const sharp = this._interpolation === 'nearest' ||
                        (this._interpolation === 'auto' && (this._nearest || Math.min(sx, sy) >= SHARP_FROM)) ||
                        (this._interpolation === 'linear' && this._nearest);
                    cr.getSource().setFilter(sharp ? Cairo.Filter.NEAREST : Cairo.Filter.GOOD);
                    cr.paint();
                } finally {
                    cr.restore();
                }
                if (this._cross) {
                    const x = Math.round(ox + (this._cross[0] + 0.5) * sx) + 0.5;
                    const y = Math.round(oy + (this._cross[1] + 0.5) * sy) + 0.5;
                    cr.setSourceRGBA(...this._crossColour, 0.7);
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
            const [, mods] = event.get_state();
            const shift = (mods & Gdk.ModifierType.SHIFT_MASK) !== 0;
            const [hasDir, direction] = event.get_scroll_direction();
            let steps = 0;
            // Some systems turn Shift + wheel into horizontal scrolling.
            if (hasDir && (direction === Gdk.ScrollDirection.UP || (shift && direction === Gdk.ScrollDirection.LEFT)))
                steps = 1;
            else if (hasDir && (direction === Gdk.ScrollDirection.DOWN || (shift && direction === Gdk.ScrollDirection.RIGHT)))
                steps = -1;
            else {
                const [ok, dx, dy] = event.get_scroll_deltas();
                if (ok) {
                    this._scrollAccum -= dy || (shift ? dx : 0);
                    steps = Math.trunc(this._scrollAccum);
                    this._scrollAccum -= steps;
                }
            }
            if (steps)
                this.emit(shift ? 'scroll-volume' : 'scroll-slice', steps);
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
            const [, state] = event.get_state();
            if (this._dragFrom && (state & Gdk.ModifierType.BUTTON3_MASK)) {
                this.emit('window-drag', x - this._dragFrom[0], y - this._dragFrom[1]);
                return true;
            }
            return this._onButton(event);
        }

        _onButton(event) {
            const [, state] = event.get_state();
            const [, button] = event.get_button();
            const type = event.get_event_type();
            if (button === 3 && type === Gdk.EventType.DOUBLE_BUTTON_PRESS) {
                this._dragFrom = null;
                this.emit('window-reset');
                return true;
            }
            if (button === 3 && type === Gdk.EventType.BUTTON_PRESS) {
                this._dragFrom = event.get_coords().slice(1);
                this.emit('window-start');
                return true;
            }
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

            this._buildGrid();
            this.add(this._grid);

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

        /** The views and the side panel (built again for another DICOM series). */
        _buildGrid() {
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
                // Wheel up = next volume, like the slider moving right.
                v.connect('scroll-volume', (w, steps) => this._state?.stepVolume(steps));
                v.connect('pick', (w, col, row) => this._pick(view, col, row));
                v.connect('hover', (w, col, row) => {
                    if (this._settings?.hover)
                        this._state?.hover(view, col < 0 ? null : col, row);
                });
                v.connect('hovered', () => {
                    this._hovered = view;
                    this._showActive();
                });
                // Right-drag: window of the grey levels (intensities only).
                v.connect('window-start', () => {
                    this._windowStart = this._state?.displayWindow ?? this._state?.stats?.window ?? null;
                });
                v.connect('window-drag', (w, dx, dy) => {
                    if (this._state?.effectiveColormap === 'gray')
                        this._state.dragWindow(this._windowStart, dx, dy);
                });
                v.connect('window-reset', () => {
                    if (this._state?.effectiveColormap === 'gray')
                        this._state.setWindow(null);
                });
                grid.attach(v, x, y, 1, 1);
                this._views[view] = v;
            }
            this._meta = new Gtk.ScrolledWindow({hscrollbar_policy: Gtk.PolicyType.NEVER, hexpand: true, vexpand: true});
            this._metaBox = new Gtk.Box({orientation: Gtk.Orientation.VERTICAL, spacing: 2});
            this._metaBox.get_style_context().add_class('np-meta');
            this._meta.add(this._metaBox);
            // The fourth quarter: the series menu and the 4D slider (when
            // there are), above the panel.
            this._side = new Gtk.Box({orientation: Gtk.Orientation.VERTICAL, spacing: 0, hexpand: true, vexpand: true});
            this._side.pack_start(this._meta, true, true, 0);
            grid.attach(this._side, 1, 1, 1, 1);
            this._volumeScale = this._volumeLabel = this._timecourse = this._timecourseArea = null;
            this._seriesRow = this._seriesCombo = null;
            this._loadButton = null;
            this._singleView = null;
            this._headerShown = false;
        }

        async _start() {
            if (this._destroyed)
                return;
            const path = this._file.get_path();
            if (!path)
                throw new Error(`This ${this._file.get_uri_scheme()}:// location has no local path: install gvfs-fuse (GNOME's bridge that gives remote files a path, often the gvfs-fuse package), or mount the server with sshfs or NFS.`);
            const [Client, State, Settings, Plot, Timecourse] = loadShared();
            this._settings = Settings.load();
            this._State = State;
            this._Settings = Settings;
            this._Plot = Plot;
            this._Timecourse = Timecourse;
            try {
                this._client = new Client.BackendClient({env: Settings.backendEnv(this._settings)});
            } catch (e) {
                throw new Error(`The GNOME NIfTI Preview backend could not be started (${e.message}). Run install.sh again.`);
            }
            this._state = this._newState();
            this._crossVisible = this._settings.crosshair === 'always';
            for (const v of Object.values(this._views))
                v.configure(this._settings);
            try {
                // Without label colours: grayscale, except the colours of RGB images.
                await this._state.open(path, this._settings.orientation, this._colormap(),
                    Settings.loadOptions(this._settings), Settings.openOptions(this._settings));
            } catch (e) {
                if (this._destroyed || e.code === 'closed')
                    return;
                if (this._state.header) {
                    // The header is valid: keep the metadata, explain the data problem.
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
                onStats: s => this._onStats(s),
                onSlice: (view, reply, s) => this._onSlice(view, reply, s),
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
            if (this._destroyed || !this._client || this._switching || uid === this._state?.header?.dicom?.series_uid)
                return;
            this._switching = true;
            const mode = this._state?.mode ?? this._settings.orientation;
            try {
                this._state?.destroy();
                if (this._panelTimer) {
                    GLib.source_remove(this._panelTimer);
                    this._panelTimer = 0;
                }
                this.remove(this._grid);
                this._grid.destroy();
                this._buildGrid();
                this.add(this._grid);
                this._grid.show_all();
                for (const v of Object.values(this._views))
                    v.configure(this._settings);
                this._error = null;
                this._spinner.show();
                this._spinner.active = true;
                this._state = this._newState();
                await this._state.openSeries(uid, mode, this._colormap(), this._Settings.loadOptions(this._settings));
            } catch (e) {
                if (!this._destroyed && e.code !== 'closed')
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
            if (this._destroyed)
                return;
            const text = this._State.remoteText(state);
            for (const v of Object.values(this._views)) {
                if (text)
                    v.showNote(text);
                else
                    v.hideNote();
            }
            if (text) {
                this._spinner.hide();
                this._spinner.active = false;
            }
            if (state.remote?.confirm && !this._loadButton) {
                const b = new Gtk.Button({label: `Load the image (${this._State.formatBytes(state.remote.bytes)} over the network)`,
                    halign: Gtk.Align.CENTER, margin_top: 6, margin_bottom: 6});
                b.get_style_context().add_class('suggested-action');
                b.connect('clicked', () => {
                    this._loadButton?.destroy();
                    this._loadButton = null;
                    this._state?.confirmLoad().catch(e => this._onError(e));
                });
                this._loadButton = b;
                this._side.pack_start(b, false, false, 0);
                this._side.reorder_child(b, this._seriesRow ? 1 : 0);
                b.show();
            } else if (!state.remote?.confirm && this._loadButton) {
                this._loadButton.destroy();
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
            if (this._destroyed || list.length < 2 || this._seriesRow)
                return;
            const studies = new Set(list.map(x => x.study_uid)).size;
            const row = new Gtk.Box({orientation: Gtk.Orientation.HORIZONTAL, spacing: 8});
            row.get_style_context().add_class('np-volume');
            row.pack_start(label('Series', ['np-key'], {valign: Gtk.Align.CENTER}), false, false, 0);
            const combo = new Gtk.ComboBoxText({hexpand: true, can_focus: false,
                tooltip_text: `${list.length} series in this ${this._seriesList.origin === 'dicomdir' ? 'DICOMDIR' : 'folder'} ([ and ] keys)`});
            for (const x of list)
                combo.append(x.uid, this._State.seriesLabel(x, studies > 1));
            // Long names are cut, so that the menu never widens the preview.
            const cell = combo.get_cells()[0];
            cell.set_property('ellipsize', 3 /* Pango.EllipsizeMode.END */);
            cell.set_property('width-chars', 12);
            combo.set_active_id(state.header?.dicom?.series_uid ?? null);
            // GTK 3 changes a combo box's choice under the mouse wheel: a
            // scroll over the panel must not open another series.
            combo.add_events(Gdk.EventMask.SCROLL_MASK);
            combo.connect('scroll-event', () => true);
            combo.connect('changed', c => {
                const uid = c.get_active_id();
                if (uid)
                    this._switchSeries(uid);
            });
            row.pack_start(combo, true, true, 0);
            this._seriesRow = row;
            this._seriesCombo = combo;
            this._side.pack_start(row, false, false, 0);
            this._side.reorder_child(row, 0);
            row.show_all();
        }

        /**
         * A single slice (a radiograph, an ultrasound clip, one DICOM image):
         * its view alone on the left, the panel on the right.
         */
        _setupSingleImage(state) {
            const view = this._State.singleView(state);
            if (!view || this._singleView || !this._views[view])
                return;
            this._singleView = view;
            for (const [name, w] of Object.entries(this._views)) {
                if (name !== view) {
                    w.destroy();
                    delete this._views[name];
                }
            }
            const kept = this._views[view];
            this._grid.remove(kept);
            this._grid.remove(this._side);
            this._grid.attach(kept, 0, 0, 1, 2);
            this._grid.attach(this._side, 1, 0, 1, 2);
            this._hovered = view;
            this._grid.show_all();
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

        /**
         * A file recognised but without slices (a DICOM report, a surface
         * overlay): the panel alone, with what the backend tells of it.
         */
        _showAbout(path, error) {
            if (this._destroyed)
                return;
            this._about = true;
            this._spinner.hide();
            this._spinner.active = false;
            // A layout of its own: the panel takes the whole window.
            this.remove(this._grid);
            this._grid.destroy();
            this._views = {};
            this._grid = new Gtk.Grid({hexpand: true, vexpand: true});
            this._meta = new Gtk.ScrolledWindow({hscrollbar_policy: Gtk.PolicyType.NEVER, hexpand: true, vexpand: true});
            this._metaBox = new Gtk.Box({orientation: Gtk.Orientation.VERTICAL, spacing: 2});
            this._metaBox.get_style_context().add_class('np-meta');
            this._meta.add(this._metaBox);
            this._grid.attach(this._meta, 0, 0, 1, 1);
            this.add(this._grid);
            const title = label(GLib.path_get_basename(path), ['np-meta-title'], {xalign: 0, wrap: true,
                wrap_mode: 2 /* WORD_CHAR */, selectable: true});
            title.set_tooltip_text(path);
            this._metaBox.pack_start(title, false, false, 0);
            this._metaBox.pack_start(label(error.message, ['np-warning'], {xalign: 0, wrap: true}), false, false, 0);
            this._metaBox.pack_start(this._sectionsGrid(this._State.aboutSections(error.about)), false, false, 0);
            this._updateButtons();
            this._grid.show_all();
        }

        /** Sections two by two; a report's text takes a row of its own. */
        _sectionsGrid(sections) {
            const grid = new Gtk.Grid({column_spacing: 28, row_spacing: 4, column_homogeneous: true});
            let cell = 0;
            for (const sec of sections) {
                const box = new Gtk.Box({orientation: Gtk.Orientation.VERTICAL, spacing: 3});
                box.pack_start(label(sec.title.toUpperCase(), ['np-section'], {xalign: 0}), false, false, 0);
                if (sec.text !== undefined) {
                    box.pack_start(label(sec.text, sec.prose ? [] : ['np-mono'], {xalign: 0, selectable: true,
                        wrap: !!sec.prose, wrap_mode: 2 /* WORD_CHAR */}), false, false, 0);
                } else {
                    const rows = new Gtk.Grid({column_spacing: 10, row_spacing: 2});
                    sec.rows.forEach(([key, value], row) => {
                        rows.attach(label(key, ['np-key'], {xalign: 0, valign: Gtk.Align.START}), 0, row, 1, 1);
                        rows.attach(label(value, [], {xalign: 0, selectable: true, wrap: true, wrap_mode: 2 /* WORD_CHAR */,
                            max_width_chars: 22}), 1, row, 1, 1);
                    });
                    box.pack_start(rows, false, false, 0);
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
            this._grid.remove(this._side);
            if (state.header.mrs.single_voxel) {
                for (const v of Object.values(this._views))
                    v.destroy();
                this._views = {};
                this._grid.attach(area, 0, 0, 2, 1);
                this._grid.attach(this._side, 0, 1, 2, 1);
            } else {
                const box = new Gtk.Box({orientation: Gtk.Orientation.VERTICAL, spacing: 1, homogeneous: true});
                box.pack_start(area, true, true, 0);
                box.pack_start(this._side, true, true, 0);
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

        /**
         * A 4D image: above the panel, a slider that scrubs through the
         * volumes and the time course of the voxel under the pointer (or the
         * cursor). Clicking or dragging on the time course picks a volume.
         */
        _setupVolumes(state) {
            if (this._volumeScale || !state.browsable())
                return;
            const n = state.header.n_volumes;
            const box = new Gtk.Box({orientation: Gtk.Orientation.VERTICAL, spacing: 0});
            const row = new Gtk.Box({orientation: Gtk.Orientation.HORIZONTAL, spacing: 10});
            row.get_style_context().add_class('np-volume');
            const adjustment = new Gtk.Adjustment({lower: 0, upper: n - 1, step_increment: 1,
                page_increment: Math.max(1, Math.round(n / 10)), value: state.volumeTarget});
            // Not focusable: the arrow keys keep switching files in Sushi.
            this._volumeScale = new Gtk.Scale({orientation: Gtk.Orientation.HORIZONTAL, adjustment, draw_value: false,
                digits: 0, round_digits: 0, hexpand: true, can_focus: false});
            this._volumeScale.set_tooltip_text('Volume (Shift + wheel on a view, comma / period, Home / End)');
            this._volumeScale.connect('value-changed', scale => {
                if (!this._settingScale)
                    this._state?.setVolume(scale.get_value());
            });
            this._volumeLabel = label('', ['np-volume-text'], {xalign: 1, width_chars: 18});
            row.pack_start(this._volumeScale, true, true, 0);
            row.pack_start(this._volumeLabel, false, false, 0);
            box.pack_start(row, false, false, 0);
            if (this._settings.timecourse ?? true) {
                this._timecourse = new this._Timecourse.TimecoursePlot();
                const area = new Gtk.DrawingArea({hexpand: true, height_request: 110, can_focus: false});
                try {
                    area.get_accessible()?.set_role(imports.gi.Atk.Role.IMAGE);
                    area.get_accessible()?.set_name('Time course');
                } catch (e) {}
                area.get_style_context().add_class('np-view');
                area.add_events(Gdk.EventMask.POINTER_MOTION_MASK | Gdk.EventMask.LEAVE_NOTIFY_MASK |
                    Gdk.EventMask.BUTTON_PRESS_MASK | Gdk.EventMask.BUTTON_MOTION_MASK);
                area.connect('draw', (w, cr) => {
                    try {
                        this._timecourse.draw(cr, w.get_allocated_width(), w.get_allocated_height(), t => w.create_pango_layout(t));
                    } catch (e) {
                        logError(e, 'nifti-preview: time course');
                    } finally {
                        cr.$dispose();
                    }
                    return false;
                });
                const pickVolume = (w, event) => {
                    const k = this._timecourse.volumeAt(event.get_coords()[1], w.get_allocated_width());
                    if (k !== null)
                        this._state?.setVolume(k);
                };
                area.connect('button-press-event', (w, event) => {
                    if (event.get_button()[1] === 1)
                        pickVolume(w, event);
                    return true;
                });
                area.connect('motion-notify-event', (w, event) => {
                    this._timecourse.pointer = event.get_coords()[1];
                    if (event.get_state()[1] & Gdk.ModifierType.BUTTON1_MASK)
                        pickVolume(w, event);
                    w.queue_draw();
                    return false;
                });
                area.connect('leave-notify-event', w => {
                    this._timecourse.pointer = null;
                    w.queue_draw();
                    return false;
                });
                this._timecourseArea = area;
                box.pack_start(area, false, false, 0);
            }
            this._side.pack_start(box, false, false, 0);
            this._side.reorder_child(box, this._seriesRow ? 1 : 0);
            this._side.show_all();
            this._onVolume(state);
        }

        _onVolume(state) {
            if (this._destroyed || !this._volumeScale)
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
            if (this._destroyed || !this._timecourse)
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
            const sections = State.metadataSections(state, {pending}).filter(sec => sec.text === undefined || this._settings.affine);
            this._metaBox.pack_start(this._sectionsGrid(sections), false, false, 0);
            // Segmentations: every label with its colour, name and volume.
            const rows = State.labelRows(state);
            if (rows.length && (this._settings.panelRows ?? ['label-list']).includes('label-list')) {
                const more = state.labelList.complete ? '' : '+';
                const expander = new Gtk.Expander({label: `Labels · ${rows.length}${more}`, expanded: !!this._labelsOpen,
                    margin_top: 10});
                expander.connect('notify::expanded', e => {
                    this._labelsOpen = e.expanded;
                });
                const grid = new Gtk.Grid({column_spacing: 8, row_spacing: 1, margin_top: 4});
                rows.forEach((r, i) => {
                    grid.attach(label(r.swatch, [], {use_markup: true}), 0, i, 1, 1);
                    grid.attach(label(r.name, [], {use_markup: true, xalign: 0, selectable: true, hexpand: true, tooltip_text: `label ${r.value}`}), 1, i, 1, 1);
                    grid.attach(label(r.volume, ['np-key'], {xalign: 1}), 2, i, 1, 1);
                });
                expander.add(grid);
                this._metaBox.pack_start(expander, false, false, 0);
            }
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
            const prefs = Gtk.Button.new_from_icon_name('emblem-system-symbolic', Gtk.IconSize.MENU);
            this._buttons.preferences = prefs;
            prefs.set_relief(Gtk.ReliefStyle.NONE);
            prefs.set_tooltip_text('Preferences of GNOME NIfTI Preview');
            prefs.connect('clicked', () => {
                this.toolbar?.resetTimeout();
                openPreferences();
            });
            box.add(prefs);
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
                if (mods & (Gdk.ModifierType.CONTROL_MASK | Gdk.ModifierType.MOD1_MASK) || this._about)
                    return false;
                // Page Up/Down, Home and End act in the side panel when it has the focus.
                const focus = w.get_focus();
                if (focus && (focus === this._meta || focus.is_ancestor(this._meta)) &&
                    [Gdk.KEY_Page_Up, Gdk.KEY_Page_Down, Gdk.KEY_Home, Gdk.KEY_End].includes(keyval))
                    return false;
                const browsable = !!this._state?.browsable();
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
    mimeTypes = ['image/x-nifti', 'image/x-compressed-nifti', 'image/x-mgh', 'image/x-compressed-mgh', 'image/x-mrtrix',
        'image/x-compressed-mrtrix', 'image/x-nrrd', 'image/x-nrrd-header', 'image/x-metaimage', 'image/x-metaimage-header',
        'application/dicom'];
} catch (e) {
    logError(e, 'nifti-preview: NIfTI viewer disabled');
}
