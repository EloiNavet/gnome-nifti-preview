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
     *                  hoverCoordinates: world coordinates under the pointer}
     */
    constructor(client, callbacks, options = {}) {
        this._client = client;
        this._cb = callbacks;
        this.options = {labelVolume: true, hoverCoordinates: false, ...options};
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
        this._generation = 0;
        this._destroyed = false;
    }

    /**
     * Open a file and fetch the three centre slices. Requests are pipelined:
     * open, load and the three slices are written at once, so the backend
     * never waits for a round trip.
     */
    async open(path, mode = 'neurological', colormap = 'auto') {
        const opened = this._client.open(path, mode);
        if (colormap !== 'auto')
            this._client.setDisplay({colormap}).catch(() => {});
        const loaded = this._client.load();
        loaded.catch(() => {});
        const info = await opened;
        this.header = info.header;
        this.grid = info.grid;
        this.plans = info.plans;
        this.mode = info.mode;
        this.cursor = info.cursor;
        this._emit('onHeader', this);
        if (this.header.mrs)
            this.requestSpectrum();
        // A single-voxel spectrum has no views to show.
        if (!this.header.mrs?.single_voxel) {
            for (const view of VIEWS)
                this.requestSlice(view);
        }
        const load = await loaded;
        this.stats = load.stats;
        this.colormap = load.colormap;
        this.effectiveColormap = load.effective_colormap;
        this.colormaps = load.colormaps ?? [];
        this._emit('onStats', this);
        return this;
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
                    labelVolume: reply.label_volume_mm3 ?? null, world: reply.world};
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
        // A new slice under a still pointer is a new voxel.
        if (this._hoverAt)
            this.hover(...this._hoverAt);
    }

    destroy() {
        this._destroyed = true;
        this._cb = {};
    }
};

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
    const geometry = [['Dimensions', h.shape.slice(0, 3).join(' × ')]];
    if (h.n_volumes > 1) {
        const t = {sec: 's', msec: 'ms', usec: 'µs', unknown: ''}[h.time_units] ?? h.time_units;
        const unit = t ? ` ${t}` : '';
        geometry.push(['Volumes', `${h.n_volumes}${h.repetition_time ? ` · TR ${formatNumber(h.repetition_time)}${unit}` : ''}`]);
    }
    const unit = h.space_units === 'unknown' ? '' : ` ${h.space_units}`;
    geometry.push(['Voxel size', `${h.zooms.slice(0, 3).map(z => formatNumber(z)).join(' × ')}${unit}`]);
    geometry.push(['Orientation', h.orientation_known ? h.orientation : 'unknown']);
    if (h.obliquity_deg !== null && h.obliquity_deg > 1)
        geometry.push(['Obliquity', `${formatNumber(h.obliquity_deg, 3)}°${state.grid?.resampled ? ', resampled' : ''}`]);
    geometry.push(['Transform', h.affine_source === 'none' ? 'none' : `${h.affine_source} (${h[h.affine_source]})`]);

    const data = [['Datatype', h.datatype]];
    if (h.slope !== 1 || h.inter !== 0)
        data.push(['Scaling', `× ${formatNumber(h.slope)} + ${formatNumber(h.inter)}`]);
    const s = state.stats;
    if (s?.labels.is_labels) {
        data.push(['Labels', s.labels.binary ? 'binary mask' : String(s.labels.n_labels)]);
    } else if (s) {
        data.push(['Range', `${formatNumber(s.min)} – ${formatNumber(s.max)}`]);
        data.push(['Window', `${formatNumber(s.window.low)} – ${formatNumber(s.window.high)}`]);
    } else if (pending) {
        data.push(['Range', '…'], ['Window', '…']);
    }
    if (h.intent && h.intent !== 'none')
        data.push(['Intent', h.intent]);
    if (h.descrip)
        data.push(['Description', h.descrip]);

    const file = [['Format', `${h.format}${h.compressed ? ', gzip' : ''}`], ['Size', formatBytes(h.file_size)]];
    return [
        {title: 'Geometry', rows: geometry},
        {title: 'Data', rows: data},
        {title: 'File', rows: file},
        {title: 'Affine', text: affineText(h.affine)},
    ];
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
        if (state.options?.labelVolume !== false && Number.isFinite(r.labelVolume))
            text += ` · ${formatNumber(r.labelVolume / 1000, 3)} cm³`;
    } else {
        text = formatNumber(r.value, 4);
    }
    if (state.options?.hoverCoordinates && state.header.orientation_known && r.world)
        text += `\n${r.world.map(c => (Number.isFinite(c) ? c.toFixed(1) : '?')).join(', ')} mm`;
    return text;
}
