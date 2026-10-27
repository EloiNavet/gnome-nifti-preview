// SPDX-License-Identifier: GPL-2.0-or-later
// nifti-preview: time course of a voxel of a 4D image (fMRI, diffusion,
// perfusion...), drawn with Cairo. Toolkit independent, like spectrumPlot.js:
// the viewers give it a Cairo context and a function that makes Pango layouts.
//
// The horizontal axis is the volume index, or the time (index × TR) when the
// header gives a repetition time. A vertical line marks the volume shown;
// clicking or dragging on the plot picks a volume (`volumeAt`).

const PangoCairo = imports.gi.PangoCairo;
const Spectrum = imports.niftiPreview.spectrumPlot;

const MARGIN = {left: 12, right: 12, top: 24, bottom: 22};
const UNITS = {sec: 's', msec: 'ms', usec: 'µs'};

var TimecoursePlot = class TimecoursePlot {
    constructor() {
        this.values = null; //  one value (or null) per volume
        this.voxel = null;
        this.shown = 0; //      volume shown
        this.target = 0; //     volume wanted (slider), may still be decompressing
        this.pointer = null; // x of the pointer, null outside
        this.step = null; //    time between volumes, null: the axis counts volumes
        this.unit = '';
        this.complete = true;
        this.progress = null;
        this.message = 'Loading the time course…';
    }

    /** Data of a `timecourse` reply (see ViewState.requestTimecourse). */
    setTimecourse(tc) {
        if (!tc || tc.error) {
            this.values = null;
            this.message = tc?.error ?? 'Time course unavailable';
            return;
        }
        if (tc.voxel === null) {
            this.values = null;
            this.message = 'Outside the image';
            return;
        }
        this.values = tc.values;
        this.voxel = tc.voxel;
        this.complete = tc.complete;
        this.progress = tc.progress;
        const tr = tc.repetition_time;
        this.step = Number.isFinite(tr) && tr > 0 && UNITS[tc.time_units] ? tr : null;
        this.unit = UNITS[tc.time_units] ?? '';
        this.message = null;
    }

    setVolume(shown, target) {
        this.shown = shown;
        this.target = target;
    }

    get title() {
        if (!this.values)
            return 'Time course';
        let t = `Voxel ${this.voxel.join(', ')}`;
        if (!this.complete)
            t += this.progress !== null && this.progress < 1 ? ` · decompressing ${Math.floor(100 * this.progress)} %` : ' · partial';
        return t;
    }

    _n() {
        return this.values?.length ?? 0;
    }

    _x(i, w) {
        const n = this._n();
        const plot = w - MARGIN.left - MARGIN.right;
        return MARGIN.left + (n > 1 ? i / (n - 1) : 0.5) * plot;
    }

    /** Volume under screen x, null outside the plot. */
    volumeAt(x, w) {
        const n = this._n();
        if (!n)
            return null;
        const plot = w - MARGIN.left - MARGIN.right;
        const t = (x - MARGIN.left) / plot;
        if (t < -0.02 || t > 1.02)
            return null;
        return Math.min(Math.max(Math.round(t * (n - 1)), 0), n - 1);
    }

    _axisText(i) {
        return this.step === null ? String(i) : `${Spectrum.formatValue(i * this.step)}`;
    }

    /** Text for the pointer: volume, time and value. */
    readout(w) {
        if (this.pointer === null || !this.values)
            return '';
        const i = this.volumeAt(this.pointer, w);
        if (i === null)
            return '';
        const v = this.values[i];
        const time = this.step === null ? '' : ` · ${Spectrum.formatValue(i * this.step)} ${this.unit}`;
        return `volume ${i}${time} · ${v === null ? '–' : Spectrum.formatValue(v)}`;
    }

    _text(cr, makeLayout, text, x, y, alpha = 0.75, anchor = 'left') {
        const layout = makeLayout(text);
        const [, logical] = layout.get_pixel_extents();
        const dx = anchor === 'center' ? -logical.width / 2 : anchor === 'right' ? -logical.width : 0;
        cr.setSourceRGBA(1, 1, 1, alpha);
        cr.moveTo(Math.round(x + dx), Math.round(y));
        PangoCairo.show_layout(cr, layout);
        return logical;
    }

    draw(cr, w, h, makeLayout) {
        cr.setSourceRGB(0, 0, 0);
        cr.paint();
        this._text(cr, makeLayout, this.title, MARGIN.left, 4, 0.75);
        if (!this.values) {
            if (this.message)
                this._text(cr, makeLayout, this.message, w / 2, h / 2 - 6, 0.8, 'center');
            return;
        }
        const n = this._n();
        const plotW = w - MARGIN.left - MARGIN.right;
        const plotH = h - MARGIN.top - MARGIN.bottom;
        if (plotW < 20 || plotH < 10 || !n)
            return;
        let vmin = Infinity, vmax = -Infinity;
        for (const v of this.values) {
            if (v === null)
                continue;
            vmin = Math.min(vmin, v);
            vmax = Math.max(vmax, v);
        }
        if (!Number.isFinite(vmin)) {
            this._text(cr, makeLayout, this.complete ? 'No finite values' : 'Decompressing…', w / 2, h / 2 - 6, 0.8, 'center');
            return;
        }
        if (vmax === vmin) {
            vmax += 0.5;
            vmin -= 0.5;
        }
        const pad = (vmax - vmin) * 0.08;
        const y = v => MARGIN.top + plotH - (v - (vmin - pad)) / (vmax - vmin + 2 * pad) * plotH;

        // Ticks on the volume (or time) axis.
        cr.setLineWidth(1);
        const span = this.step === null ? n - 1 : (n - 1) * this.step;
        const tick = Spectrum.niceStep(Math.max(span, 1e-9), Math.max(2, Math.floor(plotW / 60)));
        const unitStep = this.step ?? 1;
        for (let t = 0; t <= span + tick * 1e-6; t += tick) {
            const i = t / unitStep;
            if (this.step === null && !Number.isInteger(i))
                continue;
            const x = Math.round(this._x(i, w)) + 0.5;
            cr.setSourceRGBA(1, 1, 1, 0.12);
            cr.moveTo(x, MARGIN.top);
            cr.lineTo(x, MARGIN.top + plotH);
            cr.stroke();
            this._text(cr, makeLayout, Spectrum.tickText(t, tick), x, MARGIN.top + plotH + 3, 0.65, 'center');
        }
        this._text(cr, makeLayout, this.step === null ? 'volume' : this.unit, w - MARGIN.right, 4, 0.6, 'right');
        this._text(cr, makeLayout, Spectrum.formatValue(vmax), MARGIN.left + 2, MARGIN.top, 0.5);
        this._text(cr, makeLayout, Spectrum.formatValue(vmin), MARGIN.left + 2, MARGIN.top + plotH - 14, 0.5);

        // The curve, broken where a value is missing.
        cr.save();
        cr.rectangle(MARGIN.left, MARGIN.top, plotW, plotH);
        cr.clip();
        cr.setSourceRGBA(0.56, 0.77, 1.0, 0.95);
        cr.setLineWidth(1.4);
        let drawing = false;
        for (let i = 0; i < n; i++) {
            const v = this.values[i];
            if (v === null) {
                drawing = false;
                continue;
            }
            if (drawing)
                cr.lineTo(this._x(i, w), y(v));
            else
                cr.moveTo(this._x(i, w), y(v));
            drawing = true;
        }
        cr.stroke();
        // Single points (between gaps) as dots.
        if (n <= plotW / 3) {
            for (let i = 0; i < n; i++) {
                const v = this.values[i];
                if (v === null)
                    continue;
                cr.arc(this._x(i, w), y(v), 1.6, 0, 2 * Math.PI);
                cr.fill();
            }
        }
        cr.restore();

        // The volume wanted (dashed while it is decompressing) and the one shown.
        const mark = (i, alpha, dashed) => {
            const x = Math.round(this._x(i, w)) + 0.5;
            cr.setSourceRGBA(1.0, 0.75, 0.3, alpha);
            cr.setLineWidth(1.5);
            if (dashed)
                cr.setDash([3, 3], 0);
            cr.moveTo(x, MARGIN.top);
            cr.lineTo(x, MARGIN.top + plotH);
            cr.stroke();
            cr.setDash([], 0);
            const v = this.values[i];
            if (v !== null && v !== undefined) {
                cr.arc(x, y(v), 3, 0, 2 * Math.PI);
                cr.fill();
            }
        };
        if (this.target !== this.shown && this.target < n)
            mark(this.target, 0.5, true);
        if (this.shown < n)
            mark(this.shown, 0.9, false);

        // Pointer.
        const text = this.readout(w);
        if (text) {
            const x = Math.round(this.pointer) + 0.5;
            cr.setSourceRGBA(0.38, 0.68, 1.0, 0.5);
            cr.setLineWidth(1);
            cr.moveTo(x, MARGIN.top);
            cr.lineTo(x, MARGIN.top + plotH);
            cr.stroke();
            const layout = makeLayout(text);
            const [, logical] = layout.get_pixel_extents();
            const bx = Math.min(Math.max(x + 8, 2), w - logical.width - 12);
            cr.setSourceRGBA(0, 0, 0, 0.75);
            cr.rectangle(bx - 5, MARGIN.top + 2, logical.width + 10, logical.height + 6);
            cr.fill();
            this._text(cr, makeLayout, text, bx, MARGIN.top + 5, 0.95);
        }
    }
};
