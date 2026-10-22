// Headless GJS tests for the shared client and view state, against the real
// Rust backend. Run: NIFTI_PREVIEW_BACKEND=backend/target/release/nifti-preview \
//   gjs tests/gjs/test_client.js FIXTURE_DIR
//
// FIXTURE_DIR must contain the files written by `nifti-preview-ref make-fixtures`.

const {GLib, Gio} = imports.gi;
const System = imports.system;

const ROOT = GLib.path_get_dirname(GLib.path_get_dirname(GLib.path_get_dirname(
    Gio.File.new_for_path(System.programPath).get_path())));
imports.searchPath.unshift(GLib.build_filenamev([ROOT, 'sushi', 'common']));
const {BackendClient, BackendError} = imports.niftiPreview.backendClient;
const {ViewState, screenToVoxel, voxelToScreen, metadataSections, readoutText, nextColormap} = imports.niftiPreview.viewState;
const Settings = imports.niftiPreview.settings;
const Cairo = imports.cairo;
const PangoCairo = imports.gi.PangoCairo;

const FIX = ARGV[0];
let failures = 0;
let passed = 0;

function check(cond, what) {
    if (cond) {
        passed++;
    } else {
        failures++;
        printerr(`FAIL: ${what}`);
        // Under GitHub Actions, also an annotation (readable without the raw log).
        if (GLib.getenv('GITHUB_ACTIONS'))
            print(`::error::${String(what).replace(/%/g, '%25').replace(/\r/g, '%0D').replace(/\n/g, '%0A')}`);
    }
}

function sleep(ms) {
    return new Promise(r => GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
        r();
        return GLib.SOURCE_REMOVE;
    }));
}

function processAlive(pid) {
    try {
        return !new TextDecoder().decode(GLib.file_get_contents(`/proc/${pid}/stat`)[1]).includes(') Z ');
    } catch (e) {
        return false; // gone (possibly between two checks)
    }
}

async function waitGone(pid, ms) {
    const t = GLib.get_monotonic_time();
    while (processAlive(pid) && GLib.get_monotonic_time() - t < ms * 1000)
        await sleep(20);
    return !processAlive(pid);
}

const fx = name => GLib.build_filenamev([FIX, name]);

async function testPipelinedSession() {
    const c = new BackendClient();
    await c.ready;
    check(c.hello.backend === 'rust', 'hello from the Rust backend');
    // Everything written at once, answers come back in order.
    const t0 = GLib.get_monotonic_time();
    const [open, load, a, s, co] = await Promise.all([
        c.open(fx('sub-001_T1w.nii.gz')), c.load(),
        c.slice('axial', 10), c.slice('sagittal', 20), c.slice('coronal', 30),
    ]);
    const ms = (GLib.get_monotonic_time() - t0) / 1000;
    print(`  pipelined open+load+3 slices: ${ms.toFixed(1)} ms`);
    check(open.header.format === 'NIfTI-1', 'header format');
    check(load.stats.window.status === 'ok', 'window computed');
    check(a.view === 'axial' && a.index === 10 && a.pixels.length === a.width * a.height * 3, 'axial pixels');
    check(s.view === 'sagittal' && s.pixels.length === s.bytes, 'sagittal pixels');
    check(co.view === 'coronal' && co.index === 30, 'coronal index');
    const v = await c.value([5, 6, 7]);
    check(v.world.length === 3, 'value world coords');
    let err = null;
    try {
        await c.slice('oblique', 0);
    } catch (e) {
        err = e;
    }
    check(err instanceof BackendError && err.code === 'bad_request', 'error mapped to BackendError');
    const after = await c.slice('axial', 11);
    check(after.ok && after.index === 11, 'client usable after an error');
    const pid = c.pid;
    c.close();
    check(await waitGone(pid, 2000), 'backend exits within 2 s of close()');
    let closedErr = null;
    try {
        await c.slice('axial', 1);
    } catch (e) {
        closedErr = e;
    }
    check(closedErr?.code === 'closed', 'requests after close are rejected');
}

async function testInvalidFiles() {
    for (const [name, code] of [['not-a-nifti.nii.gz', 'invalid_nifti'], ['truncated.nii.gz', 'corrupt_data']]) {
        const c = new BackendClient();
        let err = null;
        try {
            await c.open(fx(name));
            await c.load();
        } catch (e) {
            err = e;
        }
        check(err?.code === code, `${name} -> ${code} (got ${err?.code})`);
        const pid = c.pid;
        c.close();
        check(await waitGone(pid, 2000), `${name}: backend exits`);
    }
}

async function testMissingBinary() {
    let threw = false;
    try {
        new BackendClient({argv: ['/nonexistent/nifti-preview', 'serve']});
    } catch (e) {
        threw = true;
    }
    check(threw, 'missing backend binary throws at construction');
}

async function testBackendCrash() {
    const c = new BackendClient();
    await c.open(fx('sub-001_T1w.nii.gz'));
    GLib.spawn_command_line_sync(`kill -9 ${c.pid}`);
    let err = null;
    try {
        await c.slice('axial', 3);
    } catch (e) {
        err = e;
    }
    check(err?.code === 'backend_exited' || err?.code === 'closed' || err !== null, 'crash reported as an error');
    // Once the backend is gone, every later request fails at once.
    const t = GLib.get_monotonic_time();
    let later = null;
    try {
        await c.value([1, 1, 1]);
    } catch (e) {
        later = e;
    }
    check(later !== null && GLib.get_monotonic_time() - t < 1e6, `requests after the crash fail at once (${later?.code})`);
    c.close();
}

/**
 * A scripted stand-in for the backend: says hello (unless told not to), then
 * answers the n-th request with the n-th list of raw lines ('{id}' is the
 * request id, '{pad:N}' N letters). Requests beyond the script are never
 * answered.
 */
function fakeBackend(replies, {hello = true} = {}) {
    const script = `
import json, re, sys, time
replies = json.loads(sys.argv[1])
if not ${hello ? 'True' : 'False'}:
    time.sleep(60)
print(json.dumps({"event": "hello", "protocol": 1, "backend": "fake"}), flush=True)
for line in sys.stdin:
    req = json.loads(line)
    if replies:
        for out in replies.pop(0):
            out = re.sub(r"\\{pad:(\\d+)\\}", lambda m: "x" * int(m.group(1)), out.replace("{id}", str(req["id"])))
            sys.stdout.write(out + "\\n")
        sys.stdout.flush()
`;
    return new BackendClient({argv: ['python3', '-c', script, JSON.stringify(replies)]});
}

async function codeOf(promise) {
    try {
        await promise;
        return 'ok';
    } catch (e) {
        return e.code ?? String(e);
    }
}

async function testHostileBackend() {
    const M = imports.niftiPreview.backendClient;
    const slice = (w, h, bytes) => `{"id": {id}, "ok": true, "view": "axial", "index": 0, "width": ${w}, "height": ${h}, "bytes": ${bytes}}`;
    for (const [what, line] of [
        ['3 GB slice', slice(32767, 32767, 32767 * 32767 * 3)],
        ['inconsistent size', slice(2, 2, 10)],
        ['negative size', slice(-4, 2, -24)],
        ['fractional size', slice(1.5, 2, 9)],
        ['wrong id', '{"id": 99, "ok": true, "pong": true}'],
        ['null response', 'null'],
        ['array response', '[1, 2]'],
        ['not JSON', 'garbage'],
        ['9 MiB line', '{"id": {id}, "ok": true, "pong": "{pad:9437184}"}'],
    ]) {
        const c = fakeBackend([[line]]);
        const pid = c.pid;
        await c.ready; // the time Python takes to start is not what is measured
        const t = GLib.get_monotonic_time();
        const code = await codeOf(c.request('slice', {view: 'axial', index: 0}));
        check(code === 'protocol' && GLib.get_monotonic_time() - t < 2e6, `${what}: refused at once (${code})`);
        check(await codeOf(c.request('ping')) === 'protocol', `${what}: client unusable afterwards`);
        check(await waitGone(pid, 4000), `${what}: backend stopped`);
    }
    // Long lines within the limit are fine (a spectrum of 65 536 points is ~2 MiB).
    const long = fakeBackend([['{"id": {id}, "ok": true, "pong": "{pad:3145728}"}'], ['{"id": {id}, "ok": true, "pong": true}']]);
    check(await codeOf(long.request('ping')) === 'ok' && await codeOf(long.request('ping')) === 'ok', 'a 3 MiB line is read, and the next one');
    long.close();
    // Spectra must be consistent with their length.
    const spectrum = (points, real, magnitude) =>
        `{"id": {id}, "ok": true, "voxel": [0, 0, 0], "unit": "ppm", "first": 4, "last": 0, "points": ${points}, "real": ${real}, "magnitude": ${magnitude}}`;
    for (const [what, line, want] of [
        ['consistent spectrum', spectrum(3, '[1, 2, 3]', '[1, 2, 3]'), 'ok'],
        ['no spectrum outside the image', '{"id": {id}, "ok": true, "voxel": null}', 'ok'],
        ['short spectrum array', spectrum(3, '[1, 2]', '[1, 2, 3]'), 'protocol'],
        ['spectrum too long', spectrum(70000, '[]', '[]'), 'protocol'],
        ['spectrum without axis', '{"id": {id}, "ok": true, "voxel": [0, 0, 0], "points": 1, "real": [1], "magnitude": [1]}', 'protocol'],
    ]) {
        const c = fakeBackend([[line]]);
        check(await codeOf(c.spectrum([0, 0, 0])) === want, `${what}: ${want}`);
        c.close();
    }
    // Event lines answer nothing and are skipped.
    const ev = fakeBackend([['{"event": "progress"}', '{"id": {id}, "ok": true, "pong": true}'], ['{"id": {id}, "ok": true, "pong": true}']]);
    check(await codeOf(ev.request('ping')) === 'ok' && await codeOf(ev.request('ping')) === 'ok', 'event lines are ignored');
    ev.close();
    // Deadlines.
    const hello = M.HELLO_TIMEOUT_MS, sliceMs = M.REQUEST_TIMEOUT_MS.slice;
    M.HELLO_TIMEOUT_MS = 300;
    M.REQUEST_TIMEOUT_MS.slice = 300;
    try {
        const mute = fakeBackend([], {hello: false});
        const pid = mute.pid;
        check(await codeOf(mute.ready) === 'timeout', 'backend that never starts: timeout');
        check(await codeOf(mute.request('ping')) === 'timeout', 'then every request fails');
        check(await waitGone(pid, 4000), 'silent backend stopped');
        // The next backends start normally (Python can take more than 300 ms
        // to start on a busy machine); only the slice deadline stays short.
        M.HELLO_TIMEOUT_MS = hello;
        const slow = fakeBackend([['{"id": {id}, "ok": true, "header": {}}']]);
        const opened = slow.request('open', {path: '/x'});
        check(await codeOf(opened) === 'ok', 'open answered');
        check(await codeOf(slow.request('slice', {view: 'axial', index: 0})) === 'timeout', 'unanswered slice: timeout');
        // open and load have no deadline (a large file may take long).
        const patient = fakeBackend([]);
        let settled = false;
        patient.request('load').then(() => (settled = true), () => (settled = true));
        await sleep(700);
        check(!settled, 'load is not timed out');
        patient.close();
    } finally {
        M.HELLO_TIMEOUT_MS = hello;
        M.REQUEST_TIMEOUT_MS.slice = sliceMs;
    }
}

async function testCallbackErrors() {
    // An exception in a viewer callback must not stall the coalesced requests.
    const c = new BackendClient();
    let thrown = false, last = -1;
    const state = new ViewState(c, {
        onSlice: (view, reply) => {
            if (view !== 'axial')
                return;
            last = reply.index;
            if (!thrown) {
                thrown = true;
                throw new Error('deliberate test exception');
            }
        },
    });
    await state.open(fx('sub-001_T1w.nii.gz'));
    for (let i = 0; i < 6; i++)
        state.scroll('axial', 1);
    for (let i = 0; i < 200 && last !== state.sliceIndex('axial'); i++)
        await sleep(10);
    check(thrown && last === state.sliceIndex('axial'), `view still updated after a callback exception (${last})`);
    state.destroy();
    c.close();
}

async function testViewStateCoalescing() {
    const c = new BackendClient();
    const delivered = {axial: [], coronal: [], sagittal: []};
    const state = new ViewState(c, {
        onSlice: (view, reply) => delivered[view].push(reply.index),
        onError: e => check(false, `unexpected error ${e.message}`),
    });
    await state.open(fx('sub-001_T1w.nii.gz'));
    const start = state.sliceIndex('axial');
    // 40 wheel steps in a burst: only a few requests should reach the backend.
    for (let i = 0; i < 40; i++)
        state.scroll('axial', 1);
    const target = state.sliceIndex('axial');
    check(target === Math.min(start + 40, state.plan('axial').n_slices - 1), 'cursor moved by the scroll steps');
    for (let i = 0; i < 100 && delivered.axial.at(-1) !== target; i++)
        await sleep(10);
    check(delivered.axial.at(-1) === target, 'last delivered axial slice is the latest wanted');
    check(delivered.axial.length < 10, `coalesced burst (${delivered.axial.length} deliveries for 40 steps)`);

    // Scrolling "up" moves towards the label the plan announces.
    check(state.plan('axial').slice_up_label === 'S', 'axial scroll-up goes superior');

    // Click in the axial view moves the coronal and sagittal slices.
    const plan = state.plan('axial');
    state.pick('axial', 3, 5);
    const ijk = screenToVoxel(plan, 3, 5, state.cursor);
    check(ijk.every((v, i) => v === state.cursor[i]), 'pick sets the cursor');
    const [col, row] = voxelToScreen(plan, state.cursor);
    check(col === 3 && row === 5, 'crosshair round trip');
    for (let i = 0; i < 100 && (delivered.sagittal.at(-1) !== state.sliceIndex('sagittal') ||
        delivered.coronal.at(-1) !== state.sliceIndex('coronal')); i++)
        await sleep(10);
    check(delivered.sagittal.at(-1) === state.sliceIndex('sagittal'), 'sagittal follows the click');
    check(delivered.coronal.at(-1) === state.sliceIndex('coronal'), 'coronal follows the click');

    // Hover: only the value under the pointer is fetched, the last position wins.
    let readings = 0;
    state._cb.onReading = () => readings++;
    for (let x = 0; x < 30; x++)
        state.hover('axial', 10 + x, 12);
    const last = screenToVoxel(plan, 39, 12, state.cursor);
    for (let i = 0; i < 100 && !(state.reading && state.reading.ijk.every((v, k) => v === last[k])); i++)
        await sleep(10);
    check(state.reading && state.reading.ijk.every((v, k) => v === last[k]) && typeof state.reading.value === 'number',
        'value under the pointer is the latest position');
    check(readings < 10, `hover requests coalesced (${readings} readings for 30 moves)`);
    state.scroll('axial', -1);
    const below = screenToVoxel(plan, 39, 12, state.cursor);
    for (let i = 0; i < 100 && !state.reading.ijk.every((v, k) => v === below[k]); i++)
        await sleep(10);
    check(state.reading.ijk.every((v, k) => v === below[k]), 'scrolling under a still pointer updates the value');
    state.hover('axial', null);
    check(state.reading === null && readoutText(state) === '', 'readout hidden when the pointer leaves');

    // Mode switch re-fetches every view with the new plans.
    await state.setDisplay({mode: 'radiological'});
    check(state.mode === 'radiological' && state.plan('axial').labels.left === 'R', 'radiological plans');
    const sections = metadataSections(state);
    check(sections.map(x => x.title).join(',') === 'Geometry,Data,File,Affine', 'four panel sections');
    const keys = sec => sections[sec].rows.map(r => r[0]).join(',');
    check(keys(0) === 'Dimensions,Voxel size,Orientation,Transform', `geometry rows (${keys(0)})`);
    check(keys(1) === 'Datatype,Range,Window', `data rows (${keys(1)})`);
    // While the statistics are computed, their rows keep their place.
    const pending = metadataSections({...state, header: state.header, stats: null}, {pending: true});
    check(pending[1].rows.map(r => r[0]).join(',') === 'Datatype,Range,Window' && pending[1].rows[1][1] === '…',
        'pending statistics keep their rows');
    check(keys(2) === 'Format,Size', `file rows (${keys(2)})`);
    check(sections[3].text.split('\n').length === 3, 'affine as three rows');
    const fake = (value, effectiveColormap, labelVolume = null, options = {}) =>
        ({header: {orientation_known: true}, effectiveColormap, options,
            reading: {ijk: [1, 2, 3], value, labelVolume, world: [1.25, -2, 30]}});
    check(readoutText(fake(412.537, 'gray')) === '412.5', 'readout shows the intensity');
    check(readoutText(fake(null, 'gray')) === 'NaN/Inf', 'readout for a non-finite voxel');
    check(readoutText(fake(42, 'labels')) === 'label 42', 'readout shows the label of a segmentation');
    check(readoutText(fake(0, 'labels')) === '', 'nothing shown over the background');
    check(readoutText(fake(16777217, 'labels')) === 'label 16777217', 'large label identifiers in full');
    check(readoutText(fake('9007199254740993', 'labels')) === 'label 9007199254740993', 'int64 identifier stays exact on the wire');
    check(readoutText(fake('18446744073709551615', 'labels', 4000)) === 'label 18446744073709551615 · 4 cm³', 'uint64 identifier and volume stay exact');
    check(readoutText(fake('-9223372036854775808', 'gray')) === '-9223372036854775808', 'int64 minimum readout stays exact');
    const viewDescription = imports.niftiPreview.viewState.viewDescription;
    const described = viewDescription({plans: {axial: {n_slices: 45, slice_axis: 2}}, cursor: [3, 4, 22],
        sliceIndex: () => 22}, 'axial', 'label 7 · 1.5 cm³\n1.0, 2.0, 3.0 mm');
    check(described === 'slice 22 of 44; cursor at 3, 4, 22; label 7 · 1.5 cm³, 1.0, 2.0, 3.0 mm', `accessible view description (${described})`);
    check(readoutText(fake(51, 'labels', 21613)) === 'label 51 · 21.6 cm³', 'readout shows the label volume');
    check(readoutText(fake(1, 'labels', 1870225)) === 'label 1 · 1870 cm³', 'large volumes keep 3 significant digits');
    check(readoutText(fake(51, 'labels', 21613, {labelVolume: false})) === 'label 51', 'label volume can be hidden');
    check(readoutText(fake(7, 'gray', null, {hoverCoordinates: true})) === '7\n1.3, -2.0, 30.0 mm', 'optional coordinates');
    state.destroy();
    const pid = c.pid;
    c.close();
    check(await waitGone(pid, 2000), 'backend exits after view state teardown');
}

async function testObliqueAndColour() {
    const onError = e => check(false, `unexpected error ${e.message}`);
    const c = new BackendClient();
    const state = new ViewState(c, {onError});
    await state.open(fx('sub-001_T1w_oblique.nii.gz'));
    check(state.grid.resampled && state.mode === 'neurological', 'tilted image resampled onto anatomical axes');
    const axial = state.plan('axial');
    check(axial.title === 'Axial' && axial.labels.left === 'L' && axial.labels.top === 'A', 'resampled axial view');
    check(state.header.warnings.some(w => w.includes('resampled')), 'warning says the views are resampled');
    check(metadataSections(state)[0].rows.some(([k, v]) => k === 'Obliquity' && v.endsWith(', resampled')),
        'panel says resampled');
    const outside = await c.value([0, 0, 0]);
    check(outside.voxel === null && outside.value === null, 'grid corner is outside the tilted image');
    state.reading = {ijk: [0, 0, 0], voxel: null, value: null, world: outside.world};
    check(readoutText(state) === '', 'nothing shown outside the image');
    const inside = await c.value(state.cursor);
    check(Array.isArray(inside.voxel) && typeof inside.value === 'number', 'value of the nearest voxel');
    // The cursor keeps its anatomical position from grid to grid.
    const before = state.world();
    await state.setDisplay({mode: 'stored'});
    check(!state.grid.resampled && state.plan('axial').title.startsWith('Axis'), 'stored mode shows the voxel grid');
    const after = state.world();
    check(before.every((x, i) => Math.abs(x - after[i]) < 1), `cursor kept (${before} -> ${after})`);
    await state.setDisplay({mode: 'neurological'});
    check(state.grid.resampled, 'back to the resampled grid');
    state.destroy();
    c.close();

    const c2 = new BackendClient();
    const rgb = new ViewState(c2, {onError});
    await rgb.open(fx('sub-001_rgb.nii.gz'));
    check(rgb.effectiveColormap === 'rgb' && rgb.colormaps.join(',') === 'rgb,gray', 'RGB image shown in colour');
    const px = await c2.slice('axial', rgb.sliceIndex('axial'));
    let coloured = 0;
    for (let i = 0; i < px.pixels.length; i += 3)
        coloured += px.pixels[i] !== px.pixels[i + 1];
    check(px.colormap === 'rgb' && coloured > 100, `colour pixels (${coloured})`);
    const v = await c2.value(rgb.cursor);
    rgb.reading = {ijk: rgb.cursor, voxel: v.voxel, value: v.value, rgb: v.rgb, world: v.world};
    check(Array.isArray(v.rgb) && readoutText(rgb) === `R ${v.rgb[0]} · G ${v.rgb[1]} · B ${v.rgb[2]}`, 'colour readout');
    check(nextColormap(rgb) === 'gray', 'L switches colour to grayscale');
    await rgb.setDisplay({colormap: nextColormap(rgb)});
    check(rgb.effectiveColormap === 'gray' && nextColormap(rgb) === 'rgb', 'grayscale, then back to colour');
    rgb.destroy();
    c2.close();
}

async function testSpectroscopy() {
    const onError = e => check(false, `unexpected error ${e.message}`);
    const waitFor = async cond => {
        for (let i = 0; i < 300 && !cond(); i++)
            await sleep(10);
        return cond();
    };
    // Single voxel: a spectrum, no slices.
    const c = new BackendClient();
    let slices = 0;
    const state = new ViewState(c, {onError, onSlice: () => slices++});
    await state.open(fx('sub-001_svs.nii.gz'));
    check(await waitFor(() => state.spectrum), 'single-voxel spectrum received');
    const m = state.header.mrs;
    check(m?.single_voxel && m.nucleus === '1H' && m.points === 1024 && m.transients === 8, 'spectroscopy metadata');
    check(state.header.warnings.length === 0 && state.header.repetition_time === null, 'no 4D warning, no TR');
    check(state.spectrum.unit === 'ppm' && state.spectrum.real.length === 1024 && state.spectrum.default_part === 'real',
        'spectrum in ppm, real part first');
    await sleep(100);
    check(slices === 0, `no slice requested for a single voxel (${slices})`);
    const sections = metadataSections(state);
    check(sections.map(x => x.title).join(',') === 'Spectroscopy,Geometry,File,Affine', 'spectroscopy panel');
    const row = key => sections[0].rows.find(r => r[0] === key)?.[1];
    check(row('Nucleus') === '1H, 123.2 MHz (2.9 T)', `nucleus row (${row('Nucleus')})`);
    check(row('Transients') === '4 coils × 2 dynamics', `transients row (${row('Transients')})`);
    check(row('Points') === '1024 over 2 kHz' && row('Timing') === 'TE 30 ms · TR 2 s', `points and timing (${row('Points')}, ${row('Timing')})`);
    const zeroTiming = metadataSections({header: {...state.header, mrs: {...m, echo_time_s: 0, repetition_time_s: null}}});
    check(zeroTiming[0].rows.find(r => r[0] === 'Timing')?.[1] === 'TE 0 ms', 'known zero EchoTime is displayed');

    const {SpectrumPlot} = imports.niftiPreview.spectrumPlot;
    const plot = new SpectrumPlot();
    plot.setSpectrum(state.spectrum);
    check(plot.part === 'real' && plot.hasRange() && plot.rangeLabel() === '0.2–4.2 ppm', 'plot shows 0.2–4.2 ppm, real part');
    const surface = new Cairo.ImageSurface(Cairo.Format.RGB24, 600, 300);
    const cr = new Cairo.Context(surface);
    const layout = t => {
        const l = PangoCairo.create_layout(cr);
        l.set_text(t, -1);
        return l;
    };
    let drawn = true;
    try {
        plot.pointer = 300;
        plot.draw(cr, 600, 300, layout);
        plot.togglePart();
        plot.toggleRange();
        plot.draw(cr, 600, 300, layout);
    } catch (e) {
        drawn = false;
        printerr(e);
    }
    check(drawn && plot.part === 'magnitude' && plot.rangeLabel() === 'Full width', 'plot drawn, L and Z switch it');
    check(/ppm · /.test(plot.readout(600)), `readout under the pointer (${plot.readout(600)})`);
    // NAA, the tallest peak of the phantom, near 2.0 ppm in the real part.
    const s = state.spectrum;
    let best = 0;
    for (let i = 1; i < s.points; i++)
        best = s.real[i] > s.real[best] ? i : best;
    const naa = s.first + (s.last - s.first) * best / (s.points - 1);
    check(Math.abs(naa - 2.01) < 0.03, `NAA at ${naa.toFixed(2)} ppm`);
    state.destroy();
    c.close();

    // Spectroscopic imaging: maps, and the spectrum follows the cursor.
    const c2 = new BackendClient();
    const mrsi = new ViewState(c2, {onError});
    await mrsi.open(fx('sub-001_mrsi.nii.gz'));
    check(await waitFor(() => mrsi.spectrum), 'spectrum of the centre voxel');
    check(mrsi.spectrum.voxel.join(',') === mrsi.cursor.join(','), `spectrum of the cursor voxel (${mrsi.spectrum.voxel})`);
    check(mrsi.spectrum.default_part === 'magnitude' && mrsi.spectrum.default_range === null, '31P: magnitude, whole width');
    mrsi.pick('axial', 0, 0);
    const want = mrsi.cursor.join(',');
    check(await waitFor(() => mrsi.spectrum.voxel.join(',') === want), `spectrum follows a click (${mrsi.spectrum.voxel} / ${want})`);
    for (let i = 0; i < 6; i++)
        mrsi.scroll('axial', 1);
    const last = mrsi.cursor.join(',');
    check(await waitFor(() => mrsi.spectrum.voxel.join(',') === last), 'latest cursor wins');
    check(mrsi.stats.notes.join() === 'Maps show the magnitude of the first time point.', 'maps note');
    mrsi.destroy();
    c2.close();
}

async function testRepeatedOpenClose() {
    const pids = [];
    for (let i = 0; i < 20; i++) {
        const c = new BackendClient();
        const state = new ViewState(c, {});
        await state.open(fx(i % 2 ? 'sub-001_dseg.nii.gz' : 'sub 001 with spaces.nii'));
        pids.push(c.pid);
        state.destroy();
        c.close();
    }
    await sleep(200);
    let alive = 0;
    for (const pid of pids) {
        if (!await waitGone(pid, 2000))
            alive++;
    }
    check(alive === 0, `20 open/close cycles leave no backend process (${alive} alive)`);
}

async function testLabelVolume() {
    const c = new BackendClient();
    const open = await c.open(fx('sub-001_dseg.nii.gz'));
    const load = await c.load();
    check(load.stats.labels.is_labels, 'segmentation detected');
    const v = await c.value(open.cursor);
    check(v.value > 0 && v.label_voxels > 0 && v.label_volume_mm3 > 0, 'label under the cursor has a volume');
    const bg = await c.value([0, 0, 0]);
    check(bg.value === 0 && bg.label_volume_mm3 === undefined, 'no volume for the background');
    await c.open(fx('sub-001_T1w.nii.gz'));
    await c.load();
    const t1 = await c.value(open.cursor);
    check(t1.label_volume_mm3 === undefined, 'no label volume on an intensity image');
    const pid = c.pid;
    c.close();
    check(await waitGone(pid, 2000), 'backend exits');
}

function writeTemp(content) {
    const [file, stream] = Gio.File.new_tmp('nifti-preview-settings-XXXXXX.ini');
    stream.get_output_stream().write_all(typeof content === 'string' ? new TextEncoder().encode(content) : content, null);
    stream.close(null);
    return file.get_path();
}

async function testSettings() {
    const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
    check(same(Settings.load('/nonexistent/settings.ini'), Settings.DEFAULTS), 'no settings file: defaults');
    const all = Settings.load(writeTemp(`# comment
[display]
orientation = radiological
label-colors = false
label-volume = false
crosshair = never
hover = false
hover-coordinates = true
[panel]
affine = false
[performance]
max-image-mb = 512
slice-cache-mb = 8
`));
    check(same(all, {orientation: 'radiological', labelColors: false, crosshair: 'never', hover: false,
        hoverCoordinates: true, labelVolume: false, affine: false, maxImageMb: 512, sliceCacheMb: 8}), 'every setting is read');
    check(Settings.load(writeTemp('[display]\ncrosshair=true\n')).crosshair === 'always', 'crosshair = true means always');
    const bad = Settings.load(writeTemp(`[display]
orientation = upside-down
crosshair = maybe
[performance]
max-image-mb = -3
slice-cache-mb = lots
`));
    check(same(bad, Settings.DEFAULTS), 'invalid values keep the defaults');
    check(same(Settings.load(writeTemp('this is not a key file\n[[[')), Settings.DEFAULTS), 'unreadable file keeps the defaults');
    // Values GLib refuses to read as strings must not throw either.
    check(same(Settings.load(writeTemp('[display]\norientation = radio\\logical\ncrosshair = never\\\n')), Settings.DEFAULTS),
        'invalid escapes keep the defaults');
    const latin1 = Settings.load(writeTemp(new Uint8Array([...new TextEncoder().encode('[display]\norientation = '), 0xe9, 0x0a])));
    check(same(latin1, Settings.DEFAULTS), 'non-UTF-8 value keeps the defaults');
    check(Settings.load(writeTemp('[display]\ncrosshair = constructor\n')).crosshair === 'on-click', 'no prototype lookups');
    const fifo = GLib.build_filenamev([GLib.get_tmp_dir(), `np-fifo-${GLib.get_monotonic_time()}.ini`]);
    GLib.spawn_command_line_sync(`mkfifo ${fifo}`);
    check(same(Settings.load(fifo), Settings.DEFAULTS), 'a FIFO is not read (it would block)');
    GLib.unlink(fifo);
    const partial = Settings.load(writeTemp('[panel]\naffine=false\n'));
    check(partial.affine === false && partial.orientation === 'neurological' && partial.crosshair === 'on-click',
        'missing keys keep the defaults');
    const env = Settings.backendEnv(all);
    check(env.NIFTI_PREVIEW_MAX_BYTES === String(512 * 1024 * 1024) && env.NIFTI_PREVIEW_CACHE_BYTES === String(8 * 1024 * 1024),
        'performance settings become backend environment');
}

async function testSettingsApplied() {
    // Display settings: initial orientation, grayscale segmentations, no label volume.
    const c = new BackendClient();
    const slices = [];
    const state = new ViewState(c, {onSlice: (view, reply) => slices.push(reply)}, {labelVolume: false});
    await state.open(fx('sub-001_dseg.nii.gz'), 'radiological', 'gray');
    for (let i = 0; i < 100 && slices.length < 3; i++)
        await sleep(10);
    check(state.mode === 'radiological', 'initial orientation from the settings');
    check(state.effectiveColormap === 'gray' && slices.length >= 3 && slices.every(r => r.colormap === 'gray'),
        'segmentation shown in grayscale from the first slice');
    // Hover the centre of the axial view (a labelled voxel in the phantom).
    const plan = state.plan('axial');
    state.effectiveColormap = 'labels';
    state.hover('axial', plan.width / 2, plan.height / 2);
    for (let i = 0; i < 100 && !state.reading; i++)
        await sleep(10);
    check(state.reading?.value > 0 && state.reading.labelVolume > 0 && !readoutText(state).includes('cm³'),
        `label volume hidden (${readoutText(state)})`);
    state.destroy();
    c.close();
    // The environment reaches the backend (backendEnv itself is checked in testSettings):
    // with a 16 MB cap, a header announcing 32 MB of voxels shows the metadata only
    // (without it, the missing data would be reported instead).
    const path = GLib.build_filenamev([GLib.get_tmp_dir(), `np-cap-${GLib.get_monotonic_time()}.nii`]);
    const hdr = new DataView(new ArrayBuffer(352));
    hdr.setInt32(0, 348, true);
    [3, 256, 256, 256, 1, 1, 1, 1].forEach((d, i) => hdr.setInt16(40 + 2 * i, d, true));
    hdr.setInt16(70, 4, true); // int16
    hdr.setInt16(72, 16, true);
    for (let i = 0; i < 8; i++)
        hdr.setFloat32(76 + 4 * i, 1, true);
    hdr.setFloat32(108, 352, true);
    new Uint8Array(hdr.buffer).set([0x6e, 0x2b, 0x31, 0], 344); // "n+1"
    GLib.file_set_contents(path, new Uint8Array(hdr.buffer));
    let lastPid = 0;
    const codeWith = async env => {
        const client = new BackendClient({env});
        lastPid = client.pid;
        try {
            await client.open(path);
            await client.load();
            return 'ok';
        } catch (e) {
            return e.code;
        } finally {
            client.close();
        }
    };
    const capped = await codeWith({NIFTI_PREVIEW_MAX_BYTES: String(16 * 1024 * 1024)});
    const uncapped = await codeWith({});
    GLib.unlink(path);
    check(capped === 'too_large' && uncapped === 'corrupt_data', `memory cap applied (${capped}, ${uncapped})`);
    check(await waitGone(lastPid, 2000), 'backend exits');
}

async function main() {
    for (const t of [testPipelinedSession, testInvalidFiles, testMissingBinary, testBackendCrash, testHostileBackend,
        testCallbackErrors,
        testViewStateCoalescing, testObliqueAndColour, testSpectroscopy, testLabelVolume, testSettings, testSettingsApplied, testRepeatedOpenClose]) {
        print(`- ${t.name}`);
        try {
            await t();
        } catch (e) {
            failures++;
            printerr(`FAIL: ${t.name} threw ${e}\n${e.stack}`);
        }
    }
}

const loop = new GLib.MainLoop(null, false);
main().finally(() => loop.quit());
loop.run();
print(`${passed} checks passed, ${failures} failed`);
System.exit(failures ? 1 : 0);
