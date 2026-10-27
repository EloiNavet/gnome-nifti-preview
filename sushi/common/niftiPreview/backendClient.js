// SPDX-License-Identifier: GPL-2.0-or-later
// nifti-preview: client for the `nifti-preview serve` backend process.
//
// Legacy GJS module (loaded through `imports`) so that both the Sushi 46-50
// viewer and the Sushi 51 ES module can share it. Only GLib/Gio are used.
//
// Protocol (docs/PROTOCOL.md): one JSON request per line, one JSON response
// line per request, in order. A response carrying `bytes: N` is followed by N
// raw bytes (RGB8 pixels). Requests may be pipelined.

const {Gio, GLib} = imports.gi;

var PROTOCOL_VERSION = 2;
const CLOSE_GRACE_MS = 1500;
// Same limits as the backend: a slice is at most 16 megapixels, 16384 on a
// side, so its RGB payload is at most 48 MiB. Anything else is refused
// before a single byte is allocated.
var MAX_SLICE_SIDE = 16384;
var MAX_SLICE_PIXELS = 16 * 1024 * 1024;
const READ_CHUNK = 1 << 20;
// A JSON line of the backend is at most this long (a spectrum of the longest
// kind, 65 536 points, takes about 2 MiB); a longer one is a protocol error,
// refused before it is decoded. The stdout buffer holds exactly one.
var MAX_LINE_BYTES = 8 << 20;
var MAX_SPECTRUM_POINTS = 1 << 16;
// Longest time course accepted (volumes of a 4D image).
var MAX_TIMECOURSE_POINTS = 1 << 18;
// Deadlines (ms): the backend must start, and answer slice and value
// requests, within these. open and load can legitimately take longer (a
// large file on a slow disk) and are bounded by the backend's own limits.
var HELLO_TIMEOUT_MS = 10000;
var REQUEST_TIMEOUT_MS = {slice: 30000, value: 30000, spectrum: 30000, set_display: 30000, set_volume: 30000,
    timecourse: 30000, labels: 30000, ping: 10000};

var BackendError = class BackendError extends Error {
    constructor(code, message) {
        super(message);
        this.name = 'BackendError';
        this.code = code;
    }
};

/** Path of the installed backend binary (overridable for tests). */
function backendPath() {
    const override = GLib.getenv('NIFTI_PREVIEW_BACKEND');
    if (override)
        return override;
    return GLib.build_filenamev([GLib.get_user_data_dir(), 'gnome-nifti-preview', 'bin', 'nifti-preview']);
}

/**
 * Next line of `stream` (a Gio.DataInputStream whose buffer holds
 * MAX_LINE_BYTES), without its newline; null at the end of the stream.
 */
function _readLine(stream, cancellable) {
    return new Promise((resolve, reject) => {
        let scanned = 0;
        const step = () => {
            const buffer = stream.peek_buffer();
            const end = buffer.indexOf(10, scanned);
            if (end >= 0) {
                const line = new TextDecoder().decode(buffer.subarray(0, end));
                stream.skip(end + 1, null);
                resolve(line);
                return;
            }
            if (buffer.length >= MAX_LINE_BYTES) {
                reject(new BackendError('protocol', `Backend response longer than ${MAX_LINE_BYTES} bytes`));
                return;
            }
            scanned = buffer.length;
            stream.fill_async(-1, GLib.PRIORITY_DEFAULT, cancellable, (s, res) => {
                try {
                    if (s.fill_finish(res) > 0) {
                        step();
                    } else if (buffer.length) { // last line, without a newline
                        stream.skip(buffer.length, null);
                        resolve(new TextDecoder().decode(buffer));
                    } else {
                        resolve(null);
                    }
                } catch (e) {
                    reject(e);
                }
            });
        };
        step();
    });
}

/** A protocol error, or null when the slice header is consistent and within limits. */
function _sliceProblem(m) {
    const w = m.width, h = m.height;
    if (!Number.isSafeInteger(w) || !Number.isSafeInteger(h) || w < 1 || h < 1)
        return `invalid slice size ${w}×${h}`;
    if (w > MAX_SLICE_SIDE || h > MAX_SLICE_SIDE || w * h > MAX_SLICE_PIXELS)
        return `slice too large (${w}×${h})`;
    if (m.bytes !== w * h * 3)
        return `slice payload of ${m.bytes} bytes for ${w}×${h} RGB`;
    return null;
}

/** A protocol error, or null when a spectrum reply is consistent and within limits. */
function _spectrumProblem(m) {
    if (m.voxel === null)
        return null; // outside the image: no spectrum
    const n = m.points;
    if (!Number.isSafeInteger(n) || n < 1 || n > MAX_SPECTRUM_POINTS)
        return `invalid spectrum length ${n}`;
    if (!Array.isArray(m.real) || !Array.isArray(m.magnitude) || m.real.length !== n || m.magnitude.length !== n)
        return `spectrum arrays do not have ${n} points`;
    if (!Number.isFinite(m.first) || !Number.isFinite(m.last) || !['ppm', 'hz', 'points'].includes(m.unit))
        return 'invalid spectrum axis';
    return null;
}

/** A protocol error, or null when a time course reply is consistent and within limits. */
function _timecourseProblem(m) {
    if (!Array.isArray(m.values) || m.values.length > MAX_TIMECOURSE_POINTS)
        return 'invalid time course';
    if (!m.values.every(v => v === null || Number.isFinite(v)))
        return 'invalid time course value';
    return null;
}

function _readExactly(stream, count, cancellable) {
    return new Promise((resolve, reject) => {
        const out = new Uint8Array(count);
        let offset = 0;
        const step = () => {
            if (offset === count) {
                resolve(out);
                return;
            }
            stream.read_bytes_async(Math.min(count - offset, READ_CHUNK), GLib.PRIORITY_DEFAULT, cancellable, (s, res) => {
                try {
                    const bytes = s.read_bytes_finish(res);
                    const size = bytes.get_size();
                    if (size === 0) {
                        reject(new BackendError('backend_exited', 'Backend closed its output'));
                        return;
                    }
                    out.set(bytes.toArray(), offset);
                    offset += size;
                    step();
                } catch (e) {
                    reject(e);
                }
            });
        };
        step();
    });
}

var BackendClient = class BackendClient {
    /** @param {{argv?: string[], env?: Object<string, string>}} params */
    constructor(params = {}) {
        this._argv = params.argv ?? [backendPath(), 'serve'];
        this._cancellable = new Gio.Cancellable();
        this._pending = [];
        this._closed = false;
        this._exited = false;
        this._broken = null; // first protocol or timeout error: the client is unusable
        this._timer = 0;
        this._writeChain = Promise.resolve();
        this._nextId = 1;

        // Throws if the binary is missing or not executable.
        const launcher = new Gio.SubprocessLauncher({
            flags: Gio.SubprocessFlags.STDIN_PIPE | Gio.SubprocessFlags.STDOUT_PIPE,
        });
        for (const [key, value] of Object.entries(params.env ?? {}))
            launcher.setenv(key, value, true);
        this._proc = launcher.spawnv(this._argv);
        this._stdin = this._proc.get_stdin_pipe();
        this._stdout = new Gio.DataInputStream({
            base_stream: this._proc.get_stdout_pipe(),
            close_base_stream: true,
            buffer_size: MAX_LINE_BYTES,
        });

        this._killTimeout = 0;
        this._proc.wait_async(null, () => {
            this._exited = true;
            if (this._killTimeout) {
                GLib.source_remove(this._killTimeout);
                this._killTimeout = 0;
            }
            this._failAll(new BackendError(this._closed ? 'closed' : 'backend_exited',
                this._closed ? 'Preview closed' : 'The preview backend stopped unexpectedly'));
        });

        this.ready = this._withDeadline(this._readHello(), HELLO_TIMEOUT_MS, 'The preview backend did not start in time');
        this.ready.then(() => this._readLoop()).catch(e => this._break(e));
        // Keep the rejection from surfacing as unhandled; callers await requests.
        this.ready.catch(() => {});
    }

    get pid() {
        const id = this._proc.get_identifier();
        return id ? Number(id) : null;
    }

    get closed() {
        return this._closed;
    }

    _withDeadline(promise, ms, message) {
        let id = 0;
        const timeout = new Promise((resolve, reject) => {
            id = GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
                id = 0;
                reject(new BackendError('timeout', message));
                return GLib.SOURCE_REMOVE;
            });
        });
        return Promise.race([promise, timeout]).finally(() => {
            if (id)
                GLib.source_remove(id);
        });
    }

    /** Fail everything now and later, and stop the backend. */
    _break(error) {
        if (!this._broken && !this._closed)
            this._broken = error instanceof BackendError ? error : new BackendError('protocol', String(error?.message ?? error));
        this._failAll(this._broken ?? error);
        this.close();
    }

    /** Deadline for the request at the head of the queue (the one being answered). */
    _armHead() {
        if (this._timer) {
            GLib.source_remove(this._timer);
            this._timer = 0;
        }
        const head = this._pending[0];
        const ms = head && REQUEST_TIMEOUT_MS[head.op];
        if (!ms)
            return;
        this._timer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
            this._timer = 0;
            this._break(new BackendError('timeout', `The preview backend did not answer (${head.op})`));
            return GLib.SOURCE_REMOVE;
        });
    }

    async _readHello() {
        const line = await _readLine(this._stdout, this._cancellable);
        if (line === null)
            throw new BackendError('backend_exited', 'The preview backend did not start');
        const hello = JSON.parse(line);
        if (hello.event !== 'hello' || hello.protocol !== PROTOCOL_VERSION)
            throw new BackendError('protocol', `Unsupported backend protocol ${hello.protocol}`);
        this.hello = hello;
        return hello;
    }

    async _readLoop() {
        for (;;) {
            let line;
            try {
                line = await _readLine(this._stdout, this._cancellable);
            } catch (e) {
                this._break(this._closed ? new BackendError('closed', 'Preview closed') : e);
                return;
            }
            if (line === null) {
                // End of output: the backend is gone, even if its exit is not reaped yet.
                this._break(new BackendError(this._closed ? 'closed' : 'backend_exited',
                    this._closed ? 'Preview closed' : 'The preview backend stopped unexpectedly'));
                return;
            }
            let message;
            try {
                message = JSON.parse(line);
            } catch (e) {
                message = undefined;
            }
            if (typeof message !== 'object' || message === null || Array.isArray(message)) {
                this._break(new BackendError('protocol', 'Malformed backend response'));
                return;
            }
            if ('event' in message)
                continue; // notifications carry no id and answer nothing
            const entry = this._pending[0];
            if (!entry || message.id !== entry.id) {
                this._break(new BackendError('protocol', `Unexpected backend response ${message.id}`));
                return;
            }
            if (message.ok && 'bytes' in message) {
                const problem = _sliceProblem(message);
                if (problem) {
                    this._break(new BackendError('protocol', `Invalid backend response: ${problem}`));
                    return;
                }
                try {
                    message.pixels = await _readExactly(this._stdout, message.bytes, this._cancellable);
                } catch (e) {
                    this._break(this._closed ? new BackendError('closed', 'Preview closed') : e);
                    return;
                }
            }
            if (message.ok && (entry.op === 'spectrum' || entry.op === 'timecourse')) {
                const problem = entry.op === 'spectrum' ? _spectrumProblem(message) : _timecourseProblem(message);
                if (problem) {
                    this._break(new BackendError('protocol', `Invalid backend response: ${problem}`));
                    return;
                }
            }
            if (this._pending[0] !== entry)
                return; // failed meanwhile (timeout, close)
            this._pending.shift();
            this._armHead();
            if (message.ok === true) {
                entry.resolve(message);
            } else {
                const error = new BackendError(String(message.error?.code ?? 'internal'), String(message.error?.message ?? 'Unknown error'));
                // What is known of a file recognised but not shown (viewState.aboutSections).
                if (Array.isArray(message.error?.about))
                    error.about = message.error.about;
                entry.reject(error);
            }
        }
    }

    _failAll(error) {
        const pending = this._pending;
        this._pending = [];
        this._armHead();
        for (const entry of pending)
            entry.reject(error);
    }

    /** Send one request; resolves with the response object. */
    request(op, params = {}) {
        if (this._broken)
            return Promise.reject(this._broken);
        if (this._closed)
            return Promise.reject(new BackendError('closed', 'Preview closed'));
        if (this._exited)
            return Promise.reject(new BackendError('backend_exited', 'The preview backend stopped unexpectedly'));
        const message = {...params, id: this._nextId++, op};
        const promise = new Promise((resolve, reject) => this._pending.push({resolve, reject, op, id: message.id}));
        if (this._pending.length === 1)
            this._armHead();
        const data = new TextEncoder().encode(`${JSON.stringify(message)}\n`);
        this._writeChain = this._writeChain.then(() => this._write(data)).catch(e => {
            this._break(this._closed ? new BackendError('closed', 'Preview closed') : e);
        });
        return promise;
    }

    _write(data) {
        return new Promise((resolve, reject) => {
            const writeFrom = offset => {
                const chunk = new GLib.Bytes(data.subarray(offset));
                this._stdin.write_bytes_async(chunk, GLib.PRIORITY_DEFAULT, this._cancellable, (s, res) => {
                    try {
                        const n = s.write_bytes_finish(res);
                        if (offset + n < data.length)
                            writeFrom(offset + n);
                        else
                            resolve();
                    } catch (e) {
                        reject(e);
                    }
                });
            };
            writeFrom(0);
        });
    }

    /** `options`: dicom_series (false: a DICOM file alone, not its series). */
    open(path, mode, options = {}) {
        return this.request('open', {path, ...(mode ? {mode} : {}), ...options});
    }

    /** DICOM: the series of the folder (or DICOMDIR) of the image. */
    seriesList() {
        return this.request('series_list');
    }

    /** DICOM: open another series of that list; replies like `open`. */
    openSeries(uid, mode) {
        return this.request('open_series', mode ? {uid, mode} : {uid});
    }

    /** Statistics of volume 0; `params`: window_percentiles, window_volume, palette. */
    load(params = {}) {
        return this.request('load', params);
    }

    setDisplay(params) {
        return this.request('set_display', params);
    }

    slice(view, index, colormap) {
        return this.request('slice', colormap ? {view, index, colormap} : {view, index});
    }

    value(ijk) {
        return this.request('value', {ijk});
    }

    /**
     * Show volume `volume` of a 4D image. `ready: false` (with `progress`)
     * while it is being decompressed: ask again later.
     */
    setVolume(volume) {
        return this.request('set_volume', {volume});
    }

    /** The labels of the volume shown (segmentations): value, name, colour, voxels, volume. */
    labels() {
        return this.request('labels');
    }

    /** Values of the voxel under grid point `ijk` in every volume. */
    timecourse(ijk) {
        return this.request('timecourse', {ijk});
    }

    /** Spectrum of the voxel under grid point `ijk` (the centre voxel when null). */
    spectrum(ijk) {
        return this.request('spectrum', ijk ? {ijk} : {});
    }

    /**
     * Stop the backend: polite close request, then EOF on stdin, then
     * SIGKILL if it is still running after a grace period. Idempotent.
     */
    close() {
        if (this._closed)
            return;
        this._closed = true;
        if (this._timer) {
            GLib.source_remove(this._timer);
            this._timer = 0;
        }
        const stdin = this._stdin;
        const finish = () => {
            try {
                stdin.close_async(GLib.PRIORITY_DEFAULT, null, (s, res) => {
                    try {
                        s.close_finish(res);
                    } catch (e) {}
                });
            } catch (e) {}
        };
        try {
            const data = new TextEncoder().encode(`${JSON.stringify({id: 0, op: 'close'})}\n`);
            this._writeChain.then(() => this._write(data)).then(finish, finish);
        } catch (e) {
            finish();
        }
        this._failAll(new BackendError('closed', 'Preview closed'));
        if (!this._exited) {
            const proc = this._proc;
            this._killTimeout = GLib.timeout_add(GLib.PRIORITY_DEFAULT, CLOSE_GRACE_MS, () => {
                this._killTimeout = 0;
                if (!this._exited)
                    proc.force_exit();
                this._cancellable.cancel();
                return GLib.SOURCE_REMOVE;
            });
        } else {
            this._cancellable.cancel();
        }
    }
};
