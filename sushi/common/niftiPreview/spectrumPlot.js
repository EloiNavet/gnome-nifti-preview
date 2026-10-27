// SPDX-License-Identifier: GPL-2.0-or-later
// nifti-preview: spectrum plot of a spectroscopy (NIfTI-MRS) file, drawn with
// Cairo. Toolkit independent: the GTK 3 and GTK 4 viewers give it a Cairo
// context and a function that makes Pango layouts for their widget.
//
// The backend sends the spectrum on a linear axis (`first` to `last`, in ppm,
// Hz or points). In ppm and Hz, high shifts come first: they are drawn on the
// left, as usual in spectroscopy.

const PangoCairo = imports.gi.PangoCairo;

const UNITS = {ppm: 'ppm', hz: 'Hz', points: 'point'};
const MARGIN = {left: 14, right: 14, top: 30, bottom: 30};

/** Shift label: as many decimals as the tick step needs. */
function tickText(v, step) {
    const decimals = Math.max(0, Math.min(3, -Math.floor(Math.log10(step) + 1e-9)));
    const s = v.toFixed(decimals);
    return s === '-0' || /^-0\.0*$/.test(s) ? s.slice(1) : s;
}

/** A round tick step giving about `count` ticks over `span`. */
function niceStep(span, count) {
    const raw = span / Math.max(1, count);
    const p = 10 ** Math.floor(Math.log10(raw));
    for (const m of [1, 2, 5, 10])
        if (raw <= m * p)
            return m * p;
    return 10 * p;
}

function formatValue(x) {
    if (!Number.isFinite(x))
        return '–';
    if (x === 0)
        return '0';
    const a = Math.abs(x);
    return a >= 1e4 || a < 1e-2 ? x.toExponential(2) : Number(x.toPrecision(3)).toString();
}

var SpectrumPlot = class SpectrumPlot {
    constructor() {
        this.spectrum = null; //  reply of the `spectrum` request
        this.part = null; //      'real' or 'magnitude'
        this.full = false; //     whole width instead of the default range
        this.pointer = null; //   x of the pointer, null outside
        this.message = 'Loading the spectrum…';
        this.title = '';
    }

    setSpectrum(reply) {
        this.spectrum = reply?.real ? reply : null;
        this.message = reply?.real ? null : 'No spectrum here (outside the image)';
        if (this.spectrum && !this.part)
            this.part = this.spectrum.default_part;
    }

    setMessage(text) {
        this.spectrum = null;
        this.message = text;
    }

    togglePart() {
        this.part = this.part === 'real' ? 'magnitude' : 'real';
    }

    /** True when a default range exists (L and Z have something to do). */
    hasRange() {
        return !!this.spectrum?.default_range;
    }

    toggleRange() {
        if (this.hasRange())
            this.full = !this.full;
    }

    rangeLabel() {
        const r = this.spectrum?.default_range;
        if (!r)
            return 'Full width';
        return this.full ? 'Full width' : `${r[0]}–${r[1]} ${UNITS[this.spectrum.unit]}`;
    }

    /** [lo, hi] of the axis shown. */
    _range() {
        const s = this.spectrum;
        const whole = [Math.min(s.first, s.last), Math.max(s.first, s.last)];
        if (s.default_range && !this.full)
            return [Math.max(whole[0], s.default_range[0]), Math.min(whole[1], s.default_range[1])];
        return whole;
    }

    _axis(i) {
        const s = this.spectrum;
        return s.points > 1 ? s.first + (s.last - s.first) * i / (s.points - 1) : s.first;
    }

    /** Screen x of an axis value; high values on the left when the axis decreases. */
    _x(v, lo, hi, w) {
        const plot = w - MARGIN.left - MARGIN.right;
        const descending = this.spectrum.last < this.spectrum.first;
        const t = (v - lo) / (hi - lo || 1);
        return MARGIN.left + (descending ? 1 - t : t) * plot;
    }

    _valueAt(x, w) {
        const [lo, hi] = this._range();
        const plot = w - MARGIN.left - MARGIN.right;
        let t = (x - MARGIN.left) / plot;
        if (t < 0 || t > 1)
            return null;
        if (this.spectrum.last < this.spectrum.first)
            t = 1 - t;
        return lo + t * (hi - lo);
    }

    /** Text for the pointer: shift and value of the nearest point. */
    readout(w) {
        const s = this.spectrum;
        if (!s || this.pointer === null)
            return '';
        const v = this._valueAt(this.pointer, w);
        if (v === null)
            return '';
        const i = Math.round((v - s.first) / ((s.last - s.first) || 1) * (s.points - 1));
        if (i < 0 || i >= s.points)
            return '';
        const unit = UNITS[s.unit];
        const shift = s.unit === 'points' ? `point ${i}` : `${v.toFixed(s.unit === 'ppm' ? 2 : 0)} ${unit}`;
        return `${shift} · ${formatValue(s[this.part][i])}`;
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
        this._text(cr, makeLayout, this.title, MARGIN.left, 6, 0.75);
        const s = this.spectrum;
        if (!s) {
            if (this.message)
                this._text(cr, makeLayout, this.message, w / 2, h / 2 - 8, 0.8, 'center');
            return;
        }
        const plotW = w - MARGIN.left - MARGIN.right;
        const plotH = h - MARGIN.top - MARGIN.bottom;
        if (plotW < 20 || plotH < 20)
            return;
        const [lo, hi] = this._range();
        const values = s[this.part] ?? s.real;
        // Visible points and their extent.
        let vmin = 0, vmax = 0;
        const visible = [];
        for (let i = 0; i < s.points; i++) {
            const a = this._axis(i);
            const v = values[i];
            if (a < lo || a > hi || v === null || !Number.isFinite(v))
                continue;
            visible.push(i);
            vmin = Math.min(vmin, v);
            vmax = Math.max(vmax, v);
        }
        if (vmax === vmin)
            vmax = vmin + 1;
        const pad = (vmax - vmin) * 0.05;
        const y = v => MARGIN.top + plotH - (v - (vmin - pad)) / (vmax - vmin + 2 * pad) * plotH;

        // Ticks and grid.
        const step = niceStep(hi - lo, Math.max(2, Math.floor(plotW / 70)));
        cr.setLineWidth(1);
        for (let t = Math.ceil(lo / step) * step; t <= hi + step * 1e-6; t += step) {
            const x = Math.round(this._x(t, lo, hi, w)) + 0.5;
            cr.setSourceRGBA(1, 1, 1, 0.12);
            cr.moveTo(x, MARGIN.top);
            cr.lineTo(x, MARGIN.top + plotH);
            cr.stroke();
            this._text(cr, makeLayout, tickText(t, step), x, MARGIN.top + plotH + 6, 0.7, 'center');
        }
        this._text(cr, makeLayout, UNITS[s.unit], w - MARGIN.right, 6, 0.6, 'right');
        if (vmin < 0) {
            const y0 = Math.round(y(0)) + 0.5;
            cr.setSourceRGBA(1, 1, 1, 0.25);
            cr.moveTo(MARGIN.left, y0);
            cr.lineTo(MARGIN.left + plotW, y0);
            cr.stroke();
        }

        // The curve: a line, or a min-max envelope per pixel when points are dense.
        cr.save();
        cr.rectangle(MARGIN.left, MARGIN.top, plotW, plotH);
        cr.clip();
        cr.setSourceRGBA(0.56, 0.77, 1.0, 0.95);
        cr.setLineWidth(1.2);
        if (visible.length > 2 * plotW) {
            let column = null, low = 0, high = 0;
            const flush = () => {
                if (column !== null) {
                    cr.moveTo(column + 0.5, y(low));
                    cr.lineTo(column + 0.5, y(high) - 0.5);
                }
            };
            for (const i of visible) {
                const c = Math.floor(this._x(this._axis(i), lo, hi, w));
                if (c !== column) {
                    flush();
                    column = c;
                    low = high = values[i];
                } else {
                    low = Math.min(low, values[i]);
                    high = Math.max(high, values[i]);
                }
            }
            flush();
        } else {
            visible.forEach((i, k) => {
                const px = this._x(this._axis(i), lo, hi, w);
                if (k === 0)
                    cr.moveTo(px, y(values[i]));
                else
                    cr.lineTo(px, y(values[i]));
            });
        }
        cr.stroke();
        cr.restore();

        // Pointer.
        const text = this.readout(w);
        if (text) {
            const x = Math.round(this.pointer) + 0.5;
            cr.setSourceRGBA(0.38, 0.68, 1.0, 0.6);
            cr.moveTo(x, MARGIN.top);
            cr.lineTo(x, MARGIN.top + plotH);
            cr.stroke();
            const layout = makeLayout(text);
            const [, logical] = layout.get_pixel_extents();
            const bx = Math.min(Math.max(x + 8, 2), w - logical.width - 12);
            cr.setSourceRGBA(0, 0, 0, 0.75);
            cr.rectangle(bx - 5, MARGIN.top + 4, logical.width + 10, logical.height + 6);
            cr.fill();
            this._text(cr, makeLayout, text, bx, MARGIN.top + 7, 0.95);
        }
    }
};
