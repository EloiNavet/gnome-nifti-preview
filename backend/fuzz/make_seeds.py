"""Small synthetic seed files for the fuzz target (standard library only).

    python3 fuzz/make_seeds.py CORPUS_DIR
"""

import gzip
import json
import os
import struct
import sys


# 20° about the left-right axis, 1 x 1 x 2.5 mm: shown resampled.
OBLIQUE = (1, 0, 0, -10, 0, 0.9397, -0.8551, -12, 0, 0.3420, 2.3492, -8)


def nifti1(dims, datatype, bitpix, data, endian="<", sform=True, intent=0, srow=(1, 0, 0, -10, 0, 1, 0, -12, 0, 0, 1, -8)):
    h = bytearray(352)
    pack = lambda fmt, off, *v: struct.pack_into(endian + fmt, h, off, *v)
    pack("i", 0, 348)
    full = [len(dims)] + list(dims) + [1] * (7 - len(dims))
    pack("8h", 40, *full)
    pack("h", 68, intent)
    pack("2h", 70, datatype, bitpix)
    pack("8f", 76, 1, 1, 1, 1, 2.5, 1, 1, 1)
    pack("f", 108, 352)
    pack("f", 112, 1)
    h[123] = 2 | 8
    if sform:
        pack("h", 254, 1)
        pack("12f", 280, *srow)
    else:
        pack("h", 252, 1)
        pack("3f", 256, 0.1, 0.2, 0.3)
    h[148:160] = b"fuzz seed\0\0\0"
    h[344:348] = b"n+1\0"
    return bytes(h) + data


def mrs2(dims, datatype, bitpix, data, meta):
    """NIfTI-MRS: NIfTI-2, intent_name mrs_v0_3, dwell time 0.5 ms, JSON extension 44."""
    content = json.dumps(meta).encode()
    content += b"\0" * (-(len(content) + 8) % 16)
    ext = struct.pack("<ii", len(content) + 8, 44) + content
    h = bytearray(nifti2(dims, datatype, bitpix, b""))
    struct.pack_into("<d", h, 104 + 8 * 4, 0.0005)
    struct.pack_into("<q", h, 168, 544 + len(ext))
    h[508:516] = b"mrs_v0_3"
    h[540] = 1
    return bytes(h) + ext + data


def nifti2(dims, datatype, bitpix, data):
    h = bytearray(544)
    struct.pack_into("<i", h, 0, 540)
    h[4:12] = b"n+2\0\r\n\x1a\n"
    struct.pack_into("<2h", h, 12, datatype, bitpix)
    full = [len(dims)] + list(dims) + [1] * (7 - len(dims))
    struct.pack_into("<8q", h, 16, *full)
    struct.pack_into("<8d", h, 104, 1, 1, 1, 1, 1, 1, 1, 1)
    struct.pack_into("<q", h, 168, 544)
    struct.pack_into("<d", h, 176, 1)
    struct.pack_into("<i", h, 348, 1)
    struct.pack_into("<12d", h, 400, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0)
    return bytes(h) + data


def main(out):
    os.makedirs(out, exist_ok=True)
    ramp = bytes(i % 251 for i in range(8 * 7 * 6 * 2 * 4))
    labels = bytes((i // 20) % 4 for i in range(8 * 7 * 6))
    seeds = {
        "int16.nii": nifti1([8, 7, 6], 4, 16, ramp[: 8 * 7 * 6 * 2]),
        "uint8_labels.nii.gz": gzip.compress(nifti1([8, 7, 6], 2, 8, labels, intent=1002)),
        "float32_be.nii": nifti1([8, 7, 6], 16, 32, ramp[: 8 * 7 * 6 * 4], endian=">"),
        "qform_4d.nii.gz": gzip.compress(nifti1([4, 5, 6, 3], 4, 16, ramp[: 4 * 5 * 6 * 3 * 2], sform=False)),
        "flat_2d.nii": nifti1([16, 12], 512, 16, ramp[: 16 * 12 * 2]),
        "nifti2_f64.nii.gz": gzip.compress(nifti2([5, 4, 3], 64, 64, ramp[: 5 * 4 * 3 * 8])),
        "rgb24.nii": nifti1([4, 4, 4], 128, 24, ramp[: 4 * 4 * 4 * 3]),
        "oblique_int16.nii.gz": gzip.compress(nifti1([8, 7, 6], 4, 16, ramp[: 8 * 7 * 6 * 2], srow=OBLIQUE)),
        "oblique_rgb24.nii": nifti1([5, 4, 3], 128, 24, ramp[: 5 * 4 * 3 * 3], srow=OBLIQUE),
        "oblique_labels.nii": nifti1([8, 7, 6], 2, 8, labels, intent=1002, srow=OBLIQUE),
        "mrs_svs.nii.gz": gzip.compress(mrs2([1, 1, 1, 64, 2], 32, 64, ramp[: 64 * 2 * 8],
                                             {"SpectrometerFrequency": [123.2], "ResonantNucleus": ["1H"], "dim_5": "DIM_COIL"})),
        "mrs_mrsi.nii": mrs2([3, 2, 2, 16], 1792, 128, ramp[: 3 * 2 * 2 * 16 * 16],
                             {"SpectrometerFrequency": [49.86], "ResonantNucleus": ["31P"]}),
    }
    for name, content in seeds.items():
        with open(os.path.join(out, name), "wb") as f:
            f.write(content)


if __name__ == "__main__":
    main(sys.argv[1])
