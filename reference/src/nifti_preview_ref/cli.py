"""``nifti-preview``: debugging and benchmarking without Nautilus or Sushi."""

from __future__ import annotations

import argparse
import json
import os
import platform
import subprocess
import sys
import tempfile
import time

from . import __version__


def _cmd_info(args) -> int:
    from .errors import PreviewError
    from .volume import wire_value, NiftiVolume

    try:
        vol = NiftiVolume(args.file)
        info = vol.header_info()
        if args.data:
            st = vol.stats()
            info["stats"] = {
                "window": st.window.to_dict(),
                "labels": st.labels.to_dict(),
                "non_finite": st.non_finite,
                "min": wire_value(st.minimum, st.labels.is_labels),
                "max": wire_value(st.maximum, st.labels.is_labels),
                "notes": st.notes,
            }
        info["plans"] = {k: p.to_dict() for k, p in vol.plans(args.mode).items()}
    except PreviewError as exc:
        print(f"error [{exc.code}]: {exc.message}", file=sys.stderr)
        return 2
    if args.json:
        from .server import _clean

        print(json.dumps(_clean(info), indent=2, ensure_ascii=False))
        return 0
    print(f"{info['name']}  ({info['format']}, {'gzip' if info['compressed'] else 'uncompressed'}, {info['file_size']} bytes)")
    print(f"  shape        {' x '.join(map(str, info['shape']))}   volumes: {info['n_volumes']}")
    print(f"  spacing      {', '.join(f'{z:g}' for z in info['zooms'][:3])} {info['space_units']}")
    print(f"  datatype     {info['datatype']} ({info['dtype']}), slope {info['slope']:g}, inter {info['inter']:g}")
    print(f"  orientation  {info['orientation']}  (affine from {info['affine_source']}; "
          f"qform {info['qform']}, sform {info['sform']})")
    for row in info["affine"]:
        print("               " + "  ".join(f"{x:10.4f}" for x in row))
    for view, plan in info["plans"].items():
        lab = plan["labels"]
        print(f"  {view:<9}    {plan['width']}x{plan['height']}, {plan['n_slices']} slices; "
              f"left {lab['left']} right {lab['right']} top {lab['top']} bottom {lab['bottom']}")
    if "stats" in info:
        st = info["stats"]
        print(f"  window       [{st['window']['low']:g}, {st['window']['high']:g}] ({st['window']['status']})")
        print(f"  labels       {st['labels']}")
        for note in st["notes"]:
            print(f"  note         {note}")
    for w in info["warnings"]:
        print(f"  warning      {w}")
    return 0


def _cmd_render(args) -> int:
    from .errors import PreviewError
    from .pngwriter import write_rgb
    from .render import render_slice
    from .volume import NiftiVolume

    try:
        vol = NiftiVolume(args.file)
        plans = vol.plans(args.mode if vol.orientation_known else "stored")
        views = list(plans) if args.view == "all" else [args.view]
        for view in views:
            plan = plans[view]
            index = plan.n_slices // 2 if args.index is None else args.index
            rendered = render_slice(vol, plan, index, args.colormap)
            out = args.output if len(views) == 1 else _suffixed(args.output, view)
            write_rgb(out, rendered.width, rendered.height, rendered.rgb)
            lab = plan.to_dict()["labels"]
            print(f"{out}: {view} slice {index}/{plan.n_slices - 1} {rendered.width}x{rendered.height} "
                  f"[{rendered.colormap}] left={lab['left']} right={lab['right']} top={lab['top']} bottom={lab['bottom']}")
    except PreviewError as exc:
        print(f"error [{exc.code}]: {exc.message}", file=sys.stderr)
        return 2
    return 0


def _suffixed(path: str, suffix: str) -> str:
    root, ext = os.path.splitext(path)
    return f"{root}-{suffix}{ext or '.png'}"


def _cmd_serve(args) -> int:
    from .server import main

    return main()


def _cmd_doctor(args) -> int:
    import nibabel
    import numpy

    print(json.dumps({
        "nifti_preview_ref": __version__,
        "python": platform.python_version(),
        "executable": sys.executable,
        "nibabel": nibabel.__version__,
        "numpy": numpy.__version__,
    }))
    return 0


def _cmd_fixtures(args) -> int:
    from .synthetic import make_fixtures

    for name, path in make_fixtures(args.directory, large=not args.small).items():
        print(path)
    return 0


# ---------------------------------------------------------------- benchmark

def _bench_case(path: str, n_slices: int) -> dict:
    """Run in a fresh interpreter so peak RSS is per case."""
    import resource

    t0 = time.perf_counter()
    import base64

    import numpy as np

    from .render import render_slice
    from .volume import NiftiVolume

    t_import = time.perf_counter() - t0
    t = time.perf_counter()
    vol = NiftiVolume(path)
    plans = vol.plans("neurological")
    t_open = time.perf_counter() - t
    t = time.perf_counter()
    vol.stats()
    t_load = time.perf_counter() - t
    t = time.perf_counter()
    for plan in plans.values():
        base64.b64encode(render_slice(vol, plan, plan.n_slices // 2).rgb)
    t_first = time.perf_counter() - t
    rng = np.random.default_rng(0)
    times = []
    for i in range(n_slices):
        plan = list(plans.values())[i % 3]
        idx = int(rng.integers(0, plan.n_slices))
        t = time.perf_counter()
        base64.b64encode(render_slice(vol, plan, idx).rgb)
        times.append(time.perf_counter() - t)
    rss_kb = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    return {
        "file": os.path.basename(path),
        "shape": list(vol.shape),
        "dtype": str(vol.stored_dtype),
        "file_mb": vol.file_size / 1e6,
        "import_s": t_import,
        "open_s": t_open,
        "load_s": t_load,
        "first_three_slices_s": t_first,
        "slice_ms_mean": 1000 * float(np.mean(times)),
        "slice_ms_p95": 1000 * float(np.percentile(times, 95)),
        "peak_rss_mb": rss_kb / 1024,
    }


def _bench_full_decompress(path: str) -> float:
    import gzip

    t = time.perf_counter()
    with gzip.open(path, "rb") as fh:
        while fh.read(1 << 22):
            pass
    return time.perf_counter() - t


def _cmd_bench(args) -> int:
    if args.case:
        print(json.dumps(_bench_case(args.case, args.slices)))
        return 0
    import numpy as np

    from . import synthetic

    workdir = args.dir or tempfile.mkdtemp(prefix="np-bench-")
    os.makedirs(workdir, exist_ok=True)
    cases = []
    head, aff = synthetic.head_phantom((256, 256, 256))
    cases.append(synthetic.save(os.path.join(workdir, "t1_256_int16.nii"), head, aff, dtype=np.int16))
    cases.append(synthetic.save(os.path.join(workdir, "t1_256_int16.nii.gz"), head, aff, dtype=np.int16))
    mni, maff = synthetic.head_phantom((182, 218, 182))
    cases.append(synthetic.save(os.path.join(workdir, "mni_182_float32.nii.gz"), mni, maff, dtype=np.float32))
    bold_vol, baff = synthetic.head_phantom((96, 96, 60))
    rng = np.random.default_rng(1)
    bold = np.stack([bold_vol + rng.normal(0, 3, bold_vol.shape).astype(np.float32) for _ in range(200)], axis=-1)
    bold_path = synthetic.save(os.path.join(workdir, "bold_96x96x60x200.nii.gz"), bold, baff, dtype=np.int16,
                               zooms=(2.0, 2.0, 2.0, 1.5))
    del bold
    cases.append(bold_path)

    t = time.perf_counter()
    subprocess.run([sys.executable, "-c", "import numpy, nibabel"], check=True)
    cold_interp = time.perf_counter() - t

    results = []
    for path in cases:
        out = subprocess.run([sys.executable, "-m", "nifti_preview_ref.cli", "bench", "--case", path, "--slices", str(args.slices)],
                             check=True, capture_output=True, text=True)
        results.append(json.loads(out.stdout))
    full_4d = _bench_full_decompress(bold_path)

    machine = f"{platform.processor() or platform.machine()}, {os.cpu_count()} CPUs, Python {platform.python_version()}"
    import nibabel

    print(f"Machine: {machine}; nibabel {nibabel.__version__}, numpy {np.__version__}")
    print(f"Interpreter start + `import numpy, nibabel`: {cold_interp:.3f} s\n")
    print("| file | shape | dtype | size (MB) | open header (ms) | load + stats (s) | first 3 slices (ms) "
          "| slice mean / p95 (ms) | peak RSS (MB) |")
    print("|---|---|---|---|---|---|---|---|---|")
    for r in results:
        print(f"| {r['file']} | {'×'.join(map(str, r['shape']))} | {r['dtype']} | {r['file_mb']:.1f} | "
              f"{1000 * r['open_s']:.1f} | {r['load_s']:.2f} | {1000 * r['first_three_slices_s']:.1f} | "
              f"{r['slice_ms_mean']:.2f} / {r['slice_ms_p95']:.2f} | {r['peak_rss_mb']:.0f} |")
    bold_load = next(r["load_s"] for r in results if r["file"].startswith("bold"))
    print(f"\n4D partial read: loading volume 0 of {os.path.basename(bold_path)} took {bold_load:.2f} s; "
          f"decompressing the whole file takes {full_4d:.2f} s.")
    if not args.dir:
        for path in cases:
            os.unlink(path)
        os.rmdir(workdir)
    return 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(prog="nifti-preview-ref", description=__doc__)
    parser.add_argument("--version", action="version", version=f"nifti-preview-ref {__version__}")
    sub = parser.add_subparsers(dest="command", required=True)

    p = sub.add_parser("info", help="print header metadata and view geometry")
    p.add_argument("file")
    p.add_argument("--json", action="store_true")
    p.add_argument("--data", action="store_true", help="also load data and report window/labels")
    p.add_argument("--mode", default="neurological", choices=["neurological", "radiological", "stored"])
    p.set_defaults(func=_cmd_info)

    p = sub.add_parser("render", help="render a slice (or all three views) to PNG")
    p.add_argument("file")
    p.add_argument("-o", "--output", required=True)
    p.add_argument("--view", default="all", choices=["axial", "coronal", "sagittal", "all"])
    p.add_argument("--index", type=int)
    p.add_argument("--mode", default="neurological", choices=["neurological", "radiological", "stored"])
    p.add_argument("--colormap", default="auto", choices=["auto", "gray", "labels"])
    p.set_defaults(func=_cmd_render)

    p = sub.add_parser("serve", help="speak the plugin protocol on stdin/stdout")
    p.set_defaults(func=_cmd_serve)

    p = sub.add_parser("doctor", help="print versions of the backend and its dependencies")
    p.set_defaults(func=_cmd_doctor)

    p = sub.add_parser("make-fixtures", help="write synthetic example files")
    p.add_argument("directory")
    p.add_argument("--small", action="store_true")
    p.set_defaults(func=_cmd_fixtures)

    p = sub.add_parser("bench", help="benchmark loading and slice rendering on synthetic volumes")
    p.add_argument("--dir")
    p.add_argument("--slices", type=int, default=200)
    p.add_argument("--case", help=argparse.SUPPRESS)
    p.set_defaults(func=_cmd_bench)

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
