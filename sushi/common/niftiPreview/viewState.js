// SPDX-License-Identifier: GPL-2.0-or-later
// nifti-preview: toolkit-independent preview state (cursor, slice requests).
//
// Legacy GJS module shared by the GTK 3 and GTK 4 viewers. It keeps the
// cursor, turns scroll/click gestures into slice requests and coalesces them:
// at most one request per view is in flight, and only the latest wanted slice
// is fetched when it completes ("latest wins"), so fast scrolling never queues
// up stale work.
//
// The cursor indexes the backend's display grid (`grid`): the voxel grid, or
// for an oblique image in an anatomical mode, a grid aligned with the world
// axes. Plans, slice indices and hover positions all refer to that grid.

const {GLib} = imports.gi;

var VIEWS = ['coronal', 'sagittal', 'axial'];
var MODES = ['neurological', 'radiological', 'stored'];
// A volume or a time course still being decompressed is asked for again after this delay.
var VOLUME_POLL_MS = 100;
/** Network: wait before reading (the file may be skipped at once), then poll. */
var NETWORK_DELAY_MS = 200;
var LOAD_POLL_MS = 200;
var TIMECOURSE_POLL_MS = 250;

/** Grid indices under image pixel (col, row) of a view (inverse of the backend's extraction). */
function screenToVoxel(plan, col, row, cursor) {
    const ijk = cursor.slice();
    ijk[plan.h_axis] = plan.h_flip ? plan.width - 1 - col : col;
    ijk[plan.v_axis] = plan.v_flip ? plan.height - 1 - row : row;
    return ijk;
}

/** Image pixel (col, row) of a grid point in a view. */
function voxelToScreen(plan, ijk) {
    const h = ijk[plan.h_axis];
    const v = ijk[plan.v_axis];
    return [plan.h_flip ? plan.width - 1 - h : h, plan.v_flip ? plan.height - 1 - v : v];
}

function voxelToWorld(affine, ijk) {
    return [0, 1, 2].map(r => affine[r][0] * ijk[0] + affine[r][1] * ijk[1] + affine[r][2] * ijk[2] + affine[r][3]);
}

var ViewState = class ViewState {
    /**
     * @param client a BackendClient
     * @param callbacks {onSlice(view, response), onCursor(state), onError(error)}
     * @param options {labelVolume: volume of the label under the pointer,
     *                  hoverCoordinates: world coordinates under the pointer,
     *                  timecourse: time course of the voxel in 4D images}
     */
    constructor(client, callbacks, options = {}) {
        this._client = client;
        this._cb = callbacks;
        this.options = {labelVolume: true, hoverCoordinates: false, timecourse: true, ...options};
        this.header = null;
        this.grid = null; //     {shape, affine, resampled}
        this.plans = null;
        this.stats = null;
        this.mode = 'neurological';
        this.colormap = 'auto';
        this.effectiveColormap = 'gray';
        this.colormaps = []; //  colormaps worth offering, the default first
        this.cursor = [0, 0, 0];
        this.hoverView = null; // view under the pointer
        this.hoverIjk = null; //  grid point under the pointer
        this.reading = null; //   {ijk, voxel, value, rgb, labelVolume (mm³), world} of the last answered hover
        this.spectrum = null; //  spectroscopy: last spectrum received (for the cursor voxel)
        this.spectrumError = null;
        this._spectrumInFlight = false;
        this._spectrumDirty = false;
        this._hoverAt = null;
        this._inFlight = {};
        this._dirty = {};
        this._valueInFlight = false;
        this._valueDirty = false;
        // 4D: the volume shown, the one wanted (the slider), and while it is
        // being decompressed, the fraction done.
        this.volume = 0;
        this.volumeTarget = 0;
        this.volumeProgress = null;
        this.volumeError = null;
        this._volumeInFlight = false;
        this._volumeDirty = false;
        this._volumeTimer = 0;
        // 4D: values of the voxel under the pointer (else the cursor) in every volume.
        this.timecourse = null; // {voxel, values, complete, progress, repetition_time, time_units}
        this._timecourseInFlight = false;
        this._timecourseDirty = false;
        this._timecourseTimer = 0;
        // The window grey levels are drawn with ({low, high, source}: auto,
        // first or manual) and the one asked by a right-drag, sent latest-wins.
        this.displayWindow = null;
        this.palette = 'gray';
        this._windowWanted = undefined;
        this._windowInFlight = false;
        this._generation = 0;
        this._destroyed = false;
        // DICOM: the series of the folder or DICOMDIR, once asked for.
        this.seriesList = null;
        // A file on a network file system, until its data are read:
        // {bytes, confirm (waiting for the user), progress, rate (bytes/s)}.
        this.remote = null;
        this._loadOptions = {};
    }

    /** A 4D image whose volumes can be browsed (spectroscopy files cannot). */
    browsable() {
        return !!this.header && !this.header.mrs && this.header.n_volumes > 1;
    }

    /**
     * Open a file and fetch the three centre slices. Requests are pipelined:
     * open, load and the three slices are written at once, so the backend
     * never waits for a round trip.
     */
    async open(path, mode = 'neurological', colormap = 'auto', loadOptions = {}, openOptions = {}) {
        return this._begin(this._client.open(path, mode, openOptions), colormap, loadOptions);
    }

    /** DICOM: open another series of the folder or DICOMDIR (a fresh state). */
    async openSeries(uid, mode = 'neurological', colormap = 'auto', loadOptions = {}) {
        return this._begin(this._client.openSeries(uid, mode), colormap, loadOptions);
    }

    /** DICOM: the series next to the image ({series, origin, incomplete}). */
    async requestSeries() {
        if (!this.header?.dicom)
            return null;
        try {
            this.seriesList = await this._client.seriesList();
        } catch (e) {
            this.seriesList = null;
        }
        if (!this._destroyed)
            this._emit('onSeries', this);
        return this.seriesList;
    }

    async _begin(opened, colormap, loadOptions) {
        if (colormap !== 'auto')
            this._client.setDisplay({colormap}).catch(() => {});
        // Over a network file system nothing is read before the client asks.
        this._loadOptions = loadOptions;
        const loaded = this._client.load({...loadOptions, defer_network: true});
        loaded.catch(() => {});
        const info = await opened;
        this.header = info.header;
        this.grid = info.grid;
        this.plans = info.plans;
        this.mode = info.mode;
        this.cursor = info.cursor;
        this._openedCursor = info.cursor.slice();
        this._emit('onHeader', this);
        const network = !!this.header.storage?.network;
        if (!network)
            this._requestImage();
        const load = await loaded;
        if (load.ready === false && load.deferred) {
            this.remote = {bytes: load.bytes_to_read, confirm: false, progress: 0, rate: null};
            const limit = this.options.confirmBytes ?? 0;
            if (limit > 0 && load.bytes_to_read > limit) {
                // Metadata only, until the user agrees to read that much.
                this.remote.confirm = true;
                this._emit('onLoadState', this);
                return this;
            }
            this._emit('onLoadState', this);
            // Pressing → right away skips the file: nothing was read.
            await new Promise(resolve => GLib.timeout_add(GLib.PRIORITY_DEFAULT, NETWORK_DELAY_MS, () => {
                resolve();
                return GLib.SOURCE_REMOVE;
            }));
            if (this._destroyed)
                return this;
            return this._loadRemote();
        }
        this._loaded(load);
        return this;
    }

    /** The first slices (and the spectrum) once the data can be read. */
    _requestImage() {
        if (this.header.mrs)
            this.requestSpectrum();
        // A single-voxel spectrum has no views to show.
        if (!this.header.mrs?.single_voxel) {
            for (const view of VIEWS)
                this.requestSlice(view);
        }
    }

    /** Network: the user agrees to read the file (see `remote.confirm`). */
    confirmLoad() {
        if (!this.remote?.confirm)
            return Promise.resolve(this);
        this.remote.confirm = false;
        return this._loadRemote();
    }

    /** Network: read the data on the backend's helper thread, with progress. */
    async _loadRemote() {
        this._emit('onLoadState', this);
        let load;
        for (;;) {
            load = await this._client.load({...this._loadOptions, async: true});
            if (this._destroyed)
                return this;
            if (load.ready !== false)
                break;
            this.remote.progress = load.progress ?? 0;
            this.remote.rate = load.bytes_per_second ?? null;
            this._emit('onLoadState', this);
            await new Promise(resolve => GLib.timeout_add(GLib.PRIORITY_DEFAULT, LOAD_POLL_MS, () => {
                resolve();
                return GLib.SOURCE_REMOVE;
            }));
            if (this._destroyed)
                return this;
        }
        this.remote = null;
        this._emit('onLoadState', this);
        this._requestImage();
        this._loaded(load);
        return this;
    }

    /** What `load` answered: statistics, display, then labels and volumes. */
    _loaded(load) {
        this.stats = load.stats;
        this.colormap = load.colormap;
        this.effectiveColormap = load.effective_colormap;
        this.colormaps = load.colormaps ?? [];
        this.displayWindow = load.display_window ?? null;
        this.palette = load.palette ?? 'gray';
        this.volume = this.volumeTarget = load.volume ?? 0;
        this._emit('onStats', this);
        // A sparse segmentation starts at its labels, unless the cursor moved.
        const untouched = this.cursor.every((c, i) => c === this._openedCursor?.[i]);
        if (load.label_cursor && untouched && !this.header.mrs) {
            const next = load.label_cursor;
            const changed = VIEWS.filter(v => next[this.plans[v].slice_axis] !== this.cursor[this.plans[v].slice_axis]);
            this.cursor = next.slice();
            for (const v of changed)
                this.requestSlice(v);
            this._cursorChanged();
        }
        this.requestLabels();
        if (this.browsable()) {
            this._emit('onVolume', this);
            this.requestTimecourse();
            const n = this.header.n_volumes;
            const start = {middle: Math.floor(n / 2), last: n - 1}[this.options.startVolume];
            // A DICOM series opens on the volume of the file chosen.
            if (start && !this.header.dicom)
                this.setVolume(start);
        }
    }

    plan(view) {
        return this.plans[view];
    }

    /** Call a viewer callback; an exception there must not stall the state. */
    _emit(name, ...args) {
        try {
            this._cb[name]?.(...args);
        } catch (e) {
            logError(e, `nifti-preview: ${name}`);
        }
    }

    sliceIndex(view) {
        return this.cursor[this.plans[view].slice_axis];
    }

    world() {
        return voxelToWorld(this.grid.affine, this.cursor);
    }

    crosshair(view) {
        return voxelToScreen(this.plans[view], this.cursor);
    }

    /**
     * Move the slice of `view` by `steps` (positive = towards slice_up_label).
     * A step is one voxel thick: `slice_step` slices of a resampled grid.
     */
    scroll(view, steps) {
        const plan = this.plans?.[view];
        if (!plan || !steps)
            return;
        const axis = plan.slice_axis;
        const step = plan.slice_step ?? 1;
        const next = Math.min(Math.max(this.cursor[axis] + steps * step * plan.slice_up, 0), plan.n_slices - 1);
        if (next === this.cursor[axis])
            return;
        this.cursor[axis] = next;
        this.requestSlice(view);
        this._cursorChanged();
    }

    /** Place the cursor under image pixel (col, row) of `view`. */
    pick(view, col, row) {
        const plan = this.plans?.[view];
        if (!plan)
            return;
        col = Math.min(Math.max(Math.floor(col), 0), plan.width - 1);
        row = Math.min(Math.max(Math.floor(row), 0), plan.height - 1);
        const next = screenToVoxel(plan, col, row, this.cursor);
        const changed = VIEWS.filter(v => v !== view && next[this.plans[v].slice_axis] !== this.cursor[this.plans[v].slice_axis]);
        this.cursor = next;
        for (const v of changed)
            this.requestSlice(v);
        this._cursorChanged();
    }

    /**
     * The pointer is over image pixel (col, row) of `view`; col = null when it
     * left the image. Only the value under the pointer is requested, one
     * request at a time, so moving the mouse never queues work.
     */
    hover(view, col, row) {
        const plan = this.plans?.[view];
        if (!plan || col === null) {
            this._hoverAt = null;
            if (this.hoverIjk || this.reading) {
                this.hoverView = this.hoverIjk = this.reading = null;
                this._emit('onReading', this);
                // Back to the time course of the cursor.
                this.requestTimecourse();
            }
            return;
        }
        this._hoverAt = [view, col, row];
        col = Math.min(Math.max(Math.floor(col), 0), plan.width - 1);
        row = Math.min(Math.max(Math.floor(row), 0), plan.height - 1);
        const ijk = screenToVoxel(plan, col, row, this.cursor);
        if (view === this.hoverView && this.hoverIjk && ijk.every((c, i) => c === this.hoverIjk[i]))
            return;
        this.hoverView = view;
        this.hoverIjk = ijk;
        this.requestValue();
        this.requestTimecourse();
    }

    recenter() {
        if (!this.grid)
            return;
        this.cursor = this.grid.shape.map(n => Math.floor(n / 2));
        for (const view of VIEWS)
            this.requestSlice(view);
        this._cursorChanged();
    }

    async setDisplay({mode, colormap}) {
        const params = {};
        if (mode) {
            params.mode = mode;
            // The backend moves it to the new grid (same anatomical point).
            params.cursor = this.cursor;
        }
        if (colormap)
            params.colormap = colormap;
        const reply = await this._client.setDisplay(params);
        this.mode = reply.mode;
        this.grid = reply.grid;
        this.plans = reply.plans;
        if (reply.cursor)
            this.cursor = reply.cursor;
        this.colormap = reply.colormap;
        if (reply.effective_colormap)
            this.effectiveColormap = reply.effective_colormap;
        if (reply.display_window)
            this.displayWindow = reply.display_window;
        if (reply.palette)
            this.palette = reply.palette;
        this._generation++;
        // The image under the pointer changed: wait for the next motion.
        this._hoverAt = null;
        if (this.hoverIjk || this.reading) {
            this.hoverView = this.hoverIjk = this.reading = null;
            this._emit('onReading', this);
        }
        this._emit('onHeader', this);
        for (const view of VIEWS)
            this.requestSlice(view);
        this._cursorChanged();
        return reply;
    }

    nextMode() {
        return MODES[(MODES.indexOf(this.mode) + 1) % MODES.length];
    }

    /**
     * Show grey levels between `low` and `high` (null: back to the automatic
     * window). Called on every motion of a right-drag: one request in flight,
     * the latest wanted window next.
     */
    setWindow(low, high) {
        if (!this.header || this.header.mrs)
            return;
        this._windowWanted = low === null ? null : [low, high];
        if (this._windowInFlight)
            return;
        const wanted = this._windowWanted;
        this._windowWanted = undefined;
        this._windowInFlight = true;
        this._client.setDisplay({window: wanted}).then(reply => {
            this._windowInFlight = false;
            if (this._destroyed)
                return;
            if (reply.display_window)
                this.displayWindow = reply.display_window;
            this._generation++;
            for (const view of VIEWS)
                this.requestSlice(view);
            this._emit('onStats', this);
            if (this._windowWanted !== undefined) {
                const next = this._windowWanted;
                this._windowWanted = undefined;
                this.setWindow(...(next === null ? [null, null] : next));
            }
        }, error => {
            this._windowInFlight = false;
            if (!this._destroyed && error.code !== 'closed' && error.code !== 'backend_exited')
                this._emit('onError', error, this);
        });
    }

    /**
     * Right-drag by (dx, dy) pixels from the window `start` ({low, high}):
     * right widens the window, up brightens (lowers the level), like most viewers.
     */
    dragWindow(start, dx, dy) {
        if (!start || !(start.high > start.low))
            return;
        const width = start.high - start.low;
        const newWidth = width * Math.exp(dx / 200);
        const level = (start.low + start.high) / 2 + dy * width / 300;
        this.setWindow(level - newWidth / 2, level + newWidth / 2);
    }

    /**
     * Show volume `k` (clamped) of a 4D image. The slider can call this on
     * every motion: one request is in flight at a time, only the latest
     * wanted volume is asked next, and a volume being decompressed is asked
     * again every VOLUME_POLL_MS until it is there.
     */
    setVolume(k) {
        if (!this.browsable())
            return;
        k = Math.min(Math.max(Math.round(k), 0), this.header.n_volumes - 1);
        if (k === this.volumeTarget && (k === this.volume || this._volumeInFlight || this._volumeTimer))
            return;
        this.volumeTarget = k;
        this.volumeError = null;
        this._emit('onVolume', this);
        this._requestVolume();
    }

    /** Move `delta` volumes from the one wanted. */
    stepVolume(delta) {
        if (this.browsable())
            this.setVolume(this.volumeTarget + delta);
    }

    _requestVolume() {
        if (this._destroyed)
            return;
        if (this._volumeTimer) {
            GLib.source_remove(this._volumeTimer);
            this._volumeTimer = 0;
        }
        if (this._volumeInFlight) {
            this._volumeDirty = true;
            return;
        }
        this._volumeInFlight = true;
        this._volumeDirty = false;
        const k = this.volumeTarget;
        this._client.setVolume(k).then(reply => {
            this._volumeInFlight = false;
            if (this._destroyed)
                return;
            if (reply.ready)
                this._volumeShown(reply);
            else
                this.volumeProgress = reply.progress ?? 0;
            this._emit('onVolume', this);
            if (this._volumeDirty || this.volumeTarget !== reply.volume)
                this._requestVolume();
            else if (!reply.ready)
                this._volumeTimer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, VOLUME_POLL_MS, () => {
                    this._volumeTimer = 0;
                    this._requestVolume();
                    return GLib.SOURCE_REMOVE;
                });
        }, error => {
            this._volumeInFlight = false;
            if (this._destroyed || error.code === 'closed' || error.code === 'backend_exited')
                return;
            // The volume wanted cannot be shown (truncated file...): keep the
            // one shown, say why next to the slider.
            this.volumeTarget = this.volume;
            this.volumeProgress = null;
            this.volumeError = error.message;
            this._emit('onVolume', this);
        });
    }

    _volumeShown(reply) {
        const changed = reply.shown !== this.volume;
        this.volume = reply.shown;
        this.volumeProgress = null;
        if (reply.display_window)
            this.displayWindow = reply.display_window;
        if (reply.stats) {
            this.stats = reply.stats;
            this.colormap = reply.colormap;
            this.effectiveColormap = reply.effective_colormap;
            this.colormaps = reply.colormaps ?? this.colormaps;
        }
        if (!changed)
            return;
        this.requestLabels();
        // New pixels for every view; slices in flight are for the old volume.
        this._generation++;
        for (const view of VIEWS)
            this.requestSlice(view);
        this._emit('onStats', this);
        // The value under a still pointer is that of the new volume.
        if (this._hoverAt) {
            const at = this._hoverAt;
            this.hoverIjk = null;
            this.hover(...at);
        }
    }

    /**
     * Segmentations: the labels of the volume shown, with their names (when
     * a colour table has them), colours and volumes, for the panel.
     */
    requestLabels() {
        if (this._destroyed || !this.stats?.labels?.is_labels) {
            this.labelList = null;
            return;
        }
        const volume = this.volume;
        this._client.labels().then(reply => {
            if (this._destroyed || volume !== this.volume)
                return;
            this.labelList = {labels: reply.labels ?? [], complete: reply.complete, scheme: reply.scheme};
            this._emit('onLabels', this);
        }, () => {});
    }

    /** Time (in the header's time unit) of volume `k`, null without a repetition time. */
    volumeTime(k) {
        const h = this.header;
        const tr = h?.repetition_time;
        return h && h.shape.length === 4 && Number.isFinite(tr) && tr > 0 ? k * tr : null;
    }

    /**
     * The time course of the voxel under the pointer, else of the cursor;
     * latest wins, and asked again while volumes are being decompressed.
     */
    requestTimecourse() {
        if (this._destroyed || !this.browsable() || !this.options.timecourse)
            return;
        if (this._timecourseTimer) {
            GLib.source_remove(this._timecourseTimer);
            this._timecourseTimer = 0;
        }
        if (this._timecourseInFlight) {
            this._timecourseDirty = true;
            return;
        }
        this._timecourseInFlight = true;
        this._timecourseDirty = false;
        const ijk = (this.hoverIjk ?? this.cursor).slice();
        this._client.timecourse(ijk).then(reply => {
            this._timecourseInFlight = false;
            if (this._destroyed)
                return;
            this.timecourse = {ijk, voxel: reply.voxel, values: reply.values, complete: reply.complete,
                progress: reply.progress, repetition_time: reply.repetition_time, time_units: reply.time_units};
            this._emit('onTimecourse', this);
            if (this._timecourseDirty)
                this.requestTimecourse();
            else if (!reply.complete && reply.progress < 1)
                this._timecourseTimer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, TIMECOURSE_POLL_MS, () => {
                    this._timecourseTimer = 0;
                    this.requestTimecourse();
                    return GLib.SOURCE_REMOVE;
                });
        }, error => {
            this._timecourseInFlight = false;
            if (this._destroyed || error.code === 'closed' || error.code === 'backend_exited')
                return;
            this.timecourse = {ijk, voxel: null, values: [], complete: true, error: error.message};
            this._emit('onTimecourse', this);
        });
    }

    requestSlice(view) {
        if (this._destroyed)
            return;
        if (this._inFlight[view]) {
            this._dirty[view] = true;
            return;
        }
        this._inFlight[view] = true;
        this._dirty[view] = false;
        const index = this.sliceIndex(view);
        const generation = this._generation;
        this._client.slice(view, index).then(reply => {
            this._inFlight[view] = false;
            if (this._destroyed)
                return;
            if (generation === this._generation)
                this._emit('onSlice', view, reply, this);
            if (this._dirty[view] || generation !== this._generation || reply.index !== this.sliceIndex(view))
                this.requestSlice(view);
        }, error => {
            this._inFlight[view] = false;
            if (!this._destroyed && error.code !== 'closed')
                this._emit('onError', error, this);
        });
    }

    /** Spectroscopy: the spectrum of the cursor voxel, latest wins. */
    requestSpectrum() {
        if (this._destroyed || !this.header?.mrs)
            return;
        if (this._spectrumInFlight) {
            this._spectrumDirty = true;
            return;
        }
        this._spectrumInFlight = true;
        this._spectrumDirty = false;
        const ijk = this.header.mrs.single_voxel ? null : this.cursor.slice();
        const done = () => {
            this._spectrumInFlight = false;
            if (this._destroyed)
                return false;
            if (this._spectrumDirty)
                this.requestSpectrum();
            return !this._spectrumInFlight;
        };
        this._client.spectrum(ijk).then(reply => {
            if (done()) {
                this.spectrum = reply;
                this.spectrumError = null;
                this._emit('onSpectrum', this);
            }
        }, error => {
            if (error.code === 'closed' || error.code === 'backend_exited') {
                this._spectrumInFlight = false;
                return;
            }
            if (done()) {
                this.spectrum = null;
                this.spectrumError = error.message;
                this._emit('onSpectrum', this);
            }
        });
    }

    requestValue() {
        if (this._destroyed || !this.hoverIjk)
            return;
        if (this._valueInFlight) {
            this._valueDirty = true;
            return;
        }
        this._valueInFlight = true;
        this._valueDirty = false;
        const ijk = this.hoverIjk.slice();
        const view = this.hoverView;
        const current = () => this.hoverIjk && view === this.hoverView && ijk.every((c, i) => c === this.hoverIjk[i]);
        this._client.value(ijk).then(reply => {
            this._valueInFlight = false;
            if (this._destroyed || !this.hoverIjk)
                return;
            // Only the voxel still under the pointer is shown (no stale flash).
            if (current()) {
                this.reading = {ijk, voxel: reply.voxel, value: reply.value, rgb: reply.rgb ?? null,
                    labelVolume: reply.label_volume_mm3 ?? null, labelName: reply.label_name ?? null, world: reply.world};
                this._emit('onReading', this);
            }
            if (this._valueDirty || !current())
                this.requestValue();
        }, error => {
            this._valueInFlight = false;
            if (!this._destroyed && this._valueDirty && error.code !== 'closed' && error.code !== 'backend_exited')
                this.requestValue();
        });
    }

    _cursorChanged() {
        this._emit('onCursor', this);
        if (this.header?.mrs && !this.header.mrs.single_voxel)
            this.requestSpectrum();
        if (!this._hoverAt)
            this.requestTimecourse();
        // A new slice under a still pointer is a new voxel.
        if (this._hoverAt)
            this.hover(...this._hoverAt);
    }

    destroy() {
        this._destroyed = true;
        this._cb = {};
        for (const id of [this._volumeTimer, this._timecourseTimer]) {
            if (id)
                GLib.source_remove(id);
        }
        this._volumeTimer = this._timecourseTimer = 0;
    }
};

/**
 * The view to show alone, for an image of a single slice (a radiograph, an
 * ultrasound clip, one DICOM image): the one that holds the slice whole.
 * null for an image with depth (or spectroscopy).
 */
function singleView(state) {
    const h = state.header;
    if (!h || h.mrs || !state.plans || (h.shape[2] ?? 1) !== 1)
        return null;
    for (const view of ['axial', 'coronal', 'sagittal']) {
        const p = state.plans[view];
        if (p && p.n_slices === 1 && p.width > 1 && p.height > 1)
            return view;
    }
    return null;
}

/** How a series is named in the series menu: "3 · T1 MPRAGE · MR · 176 images" (and its study). */
function seriesLabel(series, withStudy = false) {
    const parts = [];
    if (series.number !== null && series.number !== undefined)
        parts.push(String(series.number));
    parts.push(series.description || 'Series');
    if (series.modality)
        parts.push(series.modality);
    parts.push(`${series.images} image${series.images === 1 ? '' : 's'}`);
    let text = parts.join(' · ');
    if (withStudy) {
        const d = series.study_date ?? '';
        const date = /^\d{8}$/.test(d) ? `${d.slice(0, 4)}-${d.slice(4, 6)}-${d.slice(6)}` : d;
        const study = [date, series.study_description].filter(Boolean).join(' ');
        if (study)
            text = `${study} — ${text}`;
    }
    return text;
}

/**
 * Network: what the views say while a file is not read yet, or being read
 * ("Reading over the network… 34 % · 12 MB/s"); null when it is read.
 */
function remoteText(state) {
    const r = state.remote;
    if (!r)
        return null;
    if (r.confirm)
        return `${formatBytes(r.bytes)} to read over the network`;
    const pct = `${Math.floor((r.progress ?? 0) * 100)} %`;
    const rate = r.rate ? ` · ${formatBytes(Math.round(r.rate))}/s` : '';
    return `Reading over the network… ${pct}${rate}`;
}

/** Unit symbol of a NIfTI time unit ('' when unknown). */
function timeUnitSymbol(unit) {
    return {sec: 's', msec: 'ms', usec: 'µs', hz: 'Hz', ppm: 'ppm', rads: 'rad/s', unknown: ''}[unit] ?? '';
}

/** Text next to the volume slider: "Volume 12 / 63 · 24 s", or the decompression progress. */
function volumeText(state) {
    const h = state.header;
    if (!h)
        return '';
    const k = state.volumeTarget;
    let text = `Volume ${k} / ${h.n_volumes - 1}`;
    const t = state.volumeTime(k);
    if (t !== null)
        text += ` · ${formatNumber(t, 4)}${timeUnitSymbol(h.time_units) ? ` ${timeUnitSymbol(h.time_units)}` : ''}`;
    if (state.volumeProgress !== null && k !== state.volume)
        text += ` · decompressing ${Math.floor(100 * state.volumeProgress)} %`;
    if (state.volumeError)
        text += ` · ${state.volumeError}`;
    return text;
}

/** "■" in the colour of a label (Pango markup), its name (or "label N") and its volume. */
function labelRows(state) {
    const list = state.labelList?.labels ?? [];
    const hex = rgb => `#${rgb.map(c => c.toString(16).padStart(2, '0')).join('')}`;
    const esc = s => GLib.markup_escape_text(String(s), -1);
    return list.map(l => ({
        swatch: `<span foreground="${hex(l.rgb)}">■</span>`,
        name: esc(l.name ?? `label ${l.label}`),
        value: String(l.label),
        volume: Number.isFinite(l.volume_mm3) ? `${formatNumber(l.volume_mm3 / 1000, 3)} cm³` : `${l.voxels} voxels`,
    }));
}

/** Toolbar names of the colormaps. */
var COLORMAP_NAMES = {labels: 'Labels', rgb: 'Colour', gray: 'Grayscale'};

/** The colormap after the current one (L key), null when there is no choice. */
function nextColormap(state) {
    const choices = state?.colormaps ?? [];
    if (choices.length < 2)
        return null;
    return choices[(choices.indexOf(state.effectiveColormap) + 1) % choices.length];
}

/** Short human-readable byte size. */
function formatBytes(n) {
    if (n < 1024)
        return `${n} B`;
    const units = ['KiB', 'MiB', 'GiB', 'TiB'];
    let v = n / 1024;
    let u = 0;
    while (v >= 1024 && u < units.length - 1) {
        v /= 1024;
        u++;
    }
    return `${v.toFixed(v < 10 ? 1 : 0)} ${units[u]}`;
}

function formatNumber(x, digits = 4) {
    if (x === null || x === undefined)
        return '–';
    if (!Number.isFinite(x))
        return String(x);
    if (Number.isInteger(x))
        return String(x);
    return Number(x.toPrecision(digits)).toString();
}

/**
 * Metadata for the side panel, in four sections laid out two by two
 * (Geometry | Data, File | Affine). Values stay short so that the panel fits
 * without scrolling. Each section is {title, rows: [[key, value]...]} or
 * {title, text} for the affine. `pending`: the statistics are still being
 * computed; their rows are kept in place with "…" so that nothing moves when
 * they arrive.
 */
function metadataSections(state, {pending = false} = {}) {
    const h = state.header;
    if (!h)
        return [];
    if (h.mrs)
        return spectroscopySections(state);
    const geometry = [['Dimensions', h.shape.slice(0, 3).join(' × '), 'dimensions']];
    if (h.n_volumes > 1) {
        const t = {sec: 's', msec: 'ms', usec: 'µs', unknown: ''}[h.time_units] ?? h.time_units;
        const unit = t ? ` ${t}` : '';
        // Frames of a cine (no position) follow each other every frame time.
        const every = h.format === 'DICOM' && !h.orientation_known ? 'every' : 'TR';
        geometry.push(['Volumes', `${h.n_volumes}${h.repetition_time ? ` · ${every} ${formatNumber(h.repetition_time)}${unit}` : ''}`, 'volumes']);
    }
    const unit = h.space_units === 'unknown' ? '' : ` ${h.space_units}`;
    geometry.push(['Voxel size', `${h.zooms.slice(0, 3).map(z => formatNumber(z)).join(' × ')}${unit}`, 'voxel-size']);
    geometry.push(['Orientation', h.orientation_known ? h.orientation : 'unknown', 'orientation']);
    if (h.obliquity_deg !== null && h.obliquity_deg > 1)
        geometry.push(['Obliquity', `${formatNumber(h.obliquity_deg, 3)}°${state.grid?.resampled ? ', resampled' : ''}`, 'obliquity']);
    const xform = h[h.affine_source];
    geometry.push(['Transform', h.affine_source === 'none' ? 'none' : `${h.affine_source}${xform ? ` (${xform})` : ''}`, 'transform']);

    const data = [['Datatype', h.datatype, 'datatype']];
    if (h.slope !== 1 || h.inter !== 0)
        data.push(['Scaling', `× ${formatNumber(h.slope)} + ${formatNumber(h.inter)}`, 'scaling']);
    const s = state.stats;
    if (s?.labels.is_labels) {
        data.push(['Labels', s.labels.binary ? 'binary mask' : String(s.labels.n_labels), 'labels']);
    } else if (s) {
        data.push(['Range', `${formatNumber(s.min)} – ${formatNumber(s.max)}`, 'range']);
        // The colours of an RGB image are shown as they are, without a window.
        if (state.effectiveColormap !== 'rgb') {
            const w = state.displayWindow ?? s.window;
            const how = {manual: ' (set by hand)', first: ' (first volume)', file: ' (from the file)'}[w.source] ?? '';
            data.push(['Window', `${formatNumber(w.low)} – ${formatNumber(w.high)}${how}`, 'window']);
        }
    } else if (pending) {
        data.push(['Range', '…', 'range'], ['Window', '…', 'window']);
    }
    if (h.intent && h.intent !== 'none')
        data.push(['Intent', h.intent, 'intent']);
    if (h.descrip)
        data.push(['Description', h.descrip, 'description']);

    const file = [['Format', `${h.format}${h.compressed ? ', gzip' : ''}`, 'format'], ['Size', formatBytes(h.file_size), 'size']];
    // Details of the format (MGH timing, MRtrix diffusion scheme, NRRD space, DICOM...).
    for (const [key, value] of h.format_fields ?? [])
        file.push([key, value, 'details']);
    const sections = [
        {title: 'Geometry', rows: geometry},
        {title: 'Data', rows: data},
        {title: 'File', rows: file},
        {title: 'Affine', text: affineText(h.affine)},
    ];
    // Rows chosen and ordered in the settings ([panel] rows).
    const order = state.options?.panelRows;
    if (Array.isArray(order)) {
        for (const sec of sections) {
            if (!sec.rows)
                continue;
            sec.rows = sec.rows.filter(r => order.includes(r[2]))
                .map((r, i) => [r, i]).sort((a, b) => order.indexOf(a[0][2]) - order.indexOf(b[0][2]) || a[1] - b[1])
                .map(([r]) => r);
        }
    }
    // DICOM: who the images are of, only when asked ([dicom] show-patient).
    if (state.options?.showPatient && h.dicom?.patient?.length)
        sections.splice(3, 0, {title: 'Patient', rows: h.dicom.patient.map(([k, v]) => [k, v, 'patient'])});
    return sections;
}

/**
 * What the backend tells of a file it recognises but has no slices of (a
 * DICOM report or radiotherapy plan, a surface overlay): sections like
 * those of metadataSections, a report's text (`prose`) on a row of its own.
 */
function aboutSections(about) {
    if (!Array.isArray(about))
        return [];
    return about.filter(s => typeof s?.title === 'string').slice(0, 12).map(s => {
        if (typeof s.text === 'string')
            return {title: s.title, text: s.text.slice(0, 50000), prose: !!s.prose};
        const rows = (Array.isArray(s.rows) ? s.rows : []).filter(r => Array.isArray(r) && r.length >= 2)
            .slice(0, 500).map(([k, v]) => [String(k), String(v)]);
        return {title: s.title, rows};
    });
}

/** Words for the NIfTI-MRS dimension tags. */
var DIM_NAMES = {
    DIM_COIL: 'coils', DIM_DYN: 'dynamics', DIM_INDIRECT_0: 'indirect', DIM_INDIRECT_1: 'indirect',
    DIM_INDIRECT_2: 'indirect', DIM_PHASE_CYCLE: 'phase cycles', DIM_EDIT: 'edits', DIM_MEAS: 'measurements',
    DIM_ISIS: 'ISIS steps',
}; // other tags (DIM_USER_0…) are shown as they are

function formatSeconds(s) {
    return s < 1 ? `${formatNumber(s * 1000, 3)} ms` : `${formatNumber(s, 3)} s`;
}

function formatHertz(hz) {
    return hz >= 1000 ? `${formatNumber(hz / 1000, 3)} kHz` : `${formatNumber(hz, 3)} Hz`;
}

/**
 * Panel of a spectroscopy file: Spectroscopy | Geometry, File | Affine.
 */
function spectroscopySections(state) {
    const h = state.header;
    const m = h.mrs;
    let frequency = m.spectrometer_mhz ? `${formatNumber(m.spectrometer_mhz, 5)} MHz` : '';
    if (m.field_t)
        frequency += ` (${formatNumber(m.field_t, 2)} T)`;
    const nucleus = m.nucleus ?? 'unknown';
    const spectro = [['Nucleus', frequency ? `${nucleus}, ${frequency}` : nucleus]];
    spectro.push(['Points', m.bandwidth_hz ? `${m.points} over ${formatHertz(m.bandwidth_hz)}` : String(m.points)]);
    if (m.dims.length)
        spectro.push(['Transients', m.dims.map(d => `${d.size} ${DIM_NAMES[d.tag] ?? d.tag}`).join(' × ')]);
    const timing = [m.echo_time_s !== null && m.echo_time_s !== undefined && `TE ${formatSeconds(m.echo_time_s)}`, m.repetition_time_s && `TR ${formatSeconds(m.repetition_time_s)}`];
    if (timing.some(Boolean))
        spectro.push(['Timing', timing.filter(Boolean).join(' · ')]);
    spectro.push(['Datatype', h.datatype]);

    const unit = h.space_units === 'unknown' ? '' : ` ${h.space_units}`;
    const geometry = m.single_voxel ? [] : [['Voxels', h.shape.slice(0, 3).join(' × ')]];
    geometry.push([m.single_voxel ? 'Voxel' : 'Voxel size', `${h.zooms.slice(0, 3).map(z => formatNumber(z)).join(' × ')}${unit}`]);
    if (h.orientation_known)
        geometry.push(['Orientation', h.orientation]);
    if (!m.single_voxel && h.obliquity_deg !== null && h.obliquity_deg > 1)
        geometry.push(['Obliquity', `${formatNumber(h.obliquity_deg, 3)}°${state.grid?.resampled ? ', resampled' : ''}`]);
    geometry.push(['Transform', h.affine_source === 'none' ? 'none' : `${h.affine_source} (${h[h.affine_source]})`]);

    const file = [['Format', `${h.format}${h.compressed ? ', gzip' : ''}`]];
    const version = /^mrs_v(\d+)_(\d+)$/.exec(m.format ?? '');
    file.push(['Standard', version ? `NIfTI-MRS ${version[1]}.${version[2]}` : 'NIfTI-MRS']);
    file.push(['Size', formatBytes(h.file_size)]);
    return [
        {title: 'Spectroscopy', rows: spectro},
        {title: 'Geometry', rows: geometry},
        {title: 'File', rows: file},
        {title: 'Affine', text: affineText(h.affine)},
    ];
}

/** The 3×4 affine, each column as wide as its widest value. */
function affineText(affine) {
    // null: not finite in the file (JSON has no NaN).
    const cell = x => (Number.isFinite(x) ? (Math.abs(x) < 5e-5 ? 0 : Number(x.toFixed(4))).toString() : '–');
    const cells = affine.slice(0, 3).map(row => row.map(cell));
    const widths = [0, 1, 2, 3].map(c => Math.max(...cells.map(row => row[c].length)));
    return cells.map(row => row.map((v, c) => v.padStart(widths[c])).join('  ')).join('\n');
}

/**
 * What a screen reader says about a view (its accessible description): the
 * slice, the cursor and the value under the pointer.
 */
function viewDescription(state, view, readout = '') {
    const plan = state.plans?.[view];
    if (!plan)
        return '';
    const parts = [`slice ${state.sliceIndex(view)} of ${plan.n_slices - 1}`, `cursor at ${state.cursor.join(', ')}`];
    if (readout)
        parts.push(readout.replace(/\n/g, ', '));
    return parts.join('; ');
}

/** Text shown next to the pointer: the value, the colour, or the label and its volume. */
function readoutText(state) {
    const r = state.reading;
    if (!r || !state.header || r.voxel === null)
        return ''; // nothing, or outside the image (resampled grid)
    let text;
    if (state.effectiveColormap === 'rgb' && r.rgb) {
        text = `R ${r.rgb[0]} · G ${r.rgb[1]} · B ${r.rgb[2]}`;
    } else if (r.value === null) {
        text = 'NaN/Inf';
    } else if (state.effectiveColormap === 'labels') {
        if (r.value === 0)
            return ''; // background
        // Integers outside the safe number range arrive as decimal strings.
        // formatNumber preserves them without converting through Number.
        text = `label ${Number.isSafeInteger(r.value) ? String(r.value) : formatNumber(r.value, 6)}`;
        if (r.labelName)
            text += ` · ${r.labelName}`;
        if (state.options?.labelVolume !== false && Number.isFinite(r.labelVolume))
            text += ` · ${formatNumber(r.labelVolume / 1000, 3)} cm³`;
    } else {
        text = formatNumber(r.value, 4);
    }
    if (state.options?.hoverCoordinates && state.header.orientation_known && r.world)
        text += `\n${r.world.map(c => (Number.isFinite(c) ? c.toFixed(1) : '?')).join(', ')} mm`;
    return text;
}
