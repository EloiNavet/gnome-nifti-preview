"""Small synthetic seed files for the fuzz targets (standard library only).

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


def dicom(elements, ts="1.2.840.10008.1.2.1"):
    """A Part 10 file in explicit VR little endian; `elements`: (group,
    element, VR, bytes), pixel data last."""
    def el(g, e, vr, v):
        if len(v) % 2:
            v += b"\0" if vr == b"UI" else b" "
        head = struct.pack("<HH", g, e) + vr
        if vr in (b"OB", b"OW", b"SQ", b"UN", b"UT"):
            n = 0xFFFFFFFF if v is None else len(v)
            return head + b"\0\0" + struct.pack("<I", n) + v
        return head + struct.pack("<H", len(v)) + v
    meta = el(2, 1, b"OB", b"\0\1") + el(2, 2, b"UI", b"1.2.840.10008.5.1.4.1.1.4") + el(2, 0x10, b"UI", ts.encode())
    out = b"\0" * 128 + b"DICM" + el(2, 0, b"UL", struct.pack("<I", len(meta))) + meta
    for g, e, vr, v in elements:
        out += el(g, e, vr, v)
    return out


def encapsulated(*fragments):
    """Encapsulated pixel data: an empty offset table, then the fragments."""
    body = struct.pack("<HHI", 0xFFFE, 0xE000, 0)
    for f in fragments:
        if len(f) % 2:
            f += b"\0"
        body += struct.pack("<HHI", 0xFFFE, 0xE000, len(f)) + f
    body += struct.pack("<HHI", 0xFFFE, 0xE0DD, 0)
    return struct.pack("<HH", 0x7FE0, 0x10) + b"OB\0\0" + struct.pack("<I", 0xFFFFFFFF) + body


def image_elements(rows, cols, bits, spp=1, photometric=b"MONOCHROME2", frames=1):
    us = lambda v: struct.pack("<H", v)
    return [
        (8, 0x60, b"CS", b"MR"), (0x20, 0x0E, b"UI", b"1.2.3"), (0x20, 0x32, b"DS", b"0\\0\\0"),
        (0x20, 0x37, b"DS", b"1\\0\\0\\0\\1\\0"), (0x28, 2, b"US", us(spp)), (0x28, 4, b"CS", photometric),
        (0x28, 8, b"IS", str(frames).encode()), (0x28, 0x10, b"US", us(rows)), (0x28, 0x11, b"US", us(cols)),
        (0x28, 0x30, b"DS", b"0.5\\0.5"), (0x28, 0x100, b"US", us(bits)), (0x28, 0x101, b"US", us(bits)),
        (0x28, 0x102, b"US", us(bits - 1)), (0x28, 0x103, b"US", us(0)),
    ]


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
    # Other formats (recognised from the content, whatever the file name).
    mif = (b"mrtrix image\ndim: 6,5,4,3\nvox: 1,2,3,nan\nlayout: -1,-2,+3,+0\ndatatype: Int16BE\n"
           b"transform: 0,1,0,5\ntransform: 1,0,0,-3\ntransform: 0,0,1,2\nfile: . 256\nEND\n").ljust(256) + ramp[: 6 * 5 * 4 * 3 * 2]
    seeds["interleaved.mif"] = mif
    seeds["bits.mif"] = b"mrtrix image\ndim: 8,7,6\nlayout: +0,+1,+2\ndatatype: Bit\nfile: . 128\nEND\n".ljust(128) + ramp[:42]
    seeds["list_first.nrrd"] = (b"NRRD0004\ntype: short\ndimension: 4\nspace: left-posterior-superior\nsizes: 3 6 5 4\n"
                                b"space directions: none (1,0,0) (0,2,0) (0,0,3)\nkinds: list domain domain domain\n"
                                b"endian: big\nencoding: gzip\nspace origin: (1,2,3)\n\n") + gzip.compress(ramp[: 6 * 5 * 4 * 3 * 2])
    seeds["ascii.nrrd"] = b"NRRD0004\ntype: short\ndimension: 3\nsizes: 3 2 2\nencoding: ascii\n\n1 2 3\n4, 5, 6\n-7 8 9 10 11 12\n"
    hello = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "src", "formats", "testdata", "hello.bz2")
    seeds["bzip2.nrrd"] = b"NRRD0004\ntype: uchar\ndimension: 3\nsizes: 3 3 2\nencoding: bzip2\n\n" + open(hello, "rb").read()
    seeds["stream_hello.bz2"] = open(hello, "rb").read()
    mgh = bytearray(284)
    struct.pack_into(">6i", mgh, 0, 1, 6, 5, 4, 3, 4)
    struct.pack_into(">h", mgh, 28, 1)
    struct.pack_into(">12f", mgh, 30, 1, 1, 1, -1, 0, 0, 0, 0, -1, 0, 1, 0)
    seeds["frames.mgz"] = gzip.compress(bytes(mgh) + ramp[: 6 * 5 * 4 * 3 * 2])
    # DICOM: native, and encapsulated streams of each codec (testdata/).
    data = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "src", "formats", "dicom", "testdata")
    stream = lambda name: open(os.path.join(data, name), "rb").read()
    seeds["native.dcm"] = dicom(image_elements(7, 8, 16, frames=2) + [(0x7FE0, 0x10, b"OW", ramp[: 7 * 8 * 2 * 2])])
    seeds["jpegls.dcm"] = dicom(image_elements(8, 8, 8), "1.2.840.10008.1.2.4.80") + encapsulated(stream("gray8.jls"))
    seeds["jpegls_rgb.dcm"] = dicom(image_elements(8, 8, 8, 3, b"RGB"), "1.2.840.10008.1.2.4.80") + encapsulated(stream("rgb_ilv2.jls"))
    seeds["jpeg.dcm"] = dicom(image_elements(8, 8, 8, 3, b"YBR_FULL_422"), "1.2.840.10008.1.2.4.50") + encapsulated(stream("rgb420.jpg"))
    seeds["j2k.dcm"] = dicom(image_elements(64, 64, 16), "1.2.840.10008.1.2.4.91") + encapsulated(stream("ramp16.j2k"))
    for name in os.listdir(data):
        seeds["stream_" + name] = stream(name)
    for name, content in seeds.items():
        with open(os.path.join(out, name), "wb") as f:
            f.write(content)


if __name__ == "__main__":
    main(sys.argv[1])
