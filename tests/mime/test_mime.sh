#!/usr/bin/env bash
# MIME routing tests in an isolated XDG_DATA_HOME.
#
# Checks that .nii/.nii.gz (and MGH/MGZ, MRtrix, NRRD) get the dedicated
# types, that a FrameMaker .mif keeps its own, that other .gz files
# and ordinary files keep their usual types, and that removing the definitions
# restores the original behaviour.
set -euo pipefail

unset CDPATH
ROOT="$(cd -- "$(dirname -- "$0")/../.." >/dev/null && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
export XDG_DATA_HOME="$WORK/data"
export XDG_DATA_DIRS="${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
export XDG_CURRENT_DESKTOP=GNOME
mkdir -p "$XDG_DATA_HOME/mime/packages" "$WORK/files"
F="$WORK/files"

# Fixtures: a minimal valid NIfTI-1 header + data, compressed and not.
python3 - "$F" <<'EOF'
import gzip, struct, sys, tarfile, io, os
d = sys.argv[1]
hdr = bytearray(352)
hdr[0:4] = struct.pack("<i", 348)
hdr[40:48] = struct.pack("<4h", 3, 2, 2, 2)
hdr[70:74] = struct.pack("<2h", 2, 8)
hdr[76:92] = struct.pack("<4f", 1, 1, 1, 1)
hdr[108:112] = struct.pack("<f", 352)
hdr[344:348] = b"n+1\0"
nii = bytes(hdr) + bytes(range(8))
open(os.path.join(d, "sub-001_T1w.nii"), "wb").write(nii)
open(os.path.join(d, "sub-001_T1w.nii.gz"), "wb").write(gzip.compress(nii))
open(os.path.join(d, "with spaces é.nii.gz"), "wb").write(gzip.compress(nii))
open(os.path.join(d, "UPPER.NII.GZ"), "wb").write(gzip.compress(nii))
open(os.path.join(d, "fake.nii.gz"), "wb").write(b"definitely not gzip")
open(os.path.join(d, "notes.txt.gz"), "wb").write(gzip.compress(b"hello\n"))
open(os.path.join(d, "plain.gz"), "wb").write(gzip.compress(b"\x00\x01binary"))
buf = io.BytesIO()
with tarfile.open(fileobj=buf, mode="w:gz") as t:
    info = tarfile.TarInfo("a.txt"); info.size = 2; t.addfile(info, io.BytesIO(b"hi"))
open(os.path.join(d, "archive.tar.gz"), "wb").write(buf.getvalue())
open(os.path.join(d, "readme.txt"), "w").write("plain text\n")
mgh = struct.pack(">7i", 1, 2, 2, 2, 1, 0, 0) + bytes(284 - 28) + bytes(8)
open(os.path.join(d, "brain.mgh"), "wb").write(mgh)
open(os.path.join(d, "aseg.mgz"), "wb").write(gzip.compress(mgh))
open(os.path.join(d, "orig.mgh.gz"), "wb").write(gzip.compress(mgh))
mif = b"mrtrix image\ndim: 2,2,2\ndatatype: UInt8\nfile: . 64\nEND\n".ljust(64) + bytes(8)
open(os.path.join(d, "dwi.mif"), "wb").write(mif)
open(os.path.join(d, "fod.mif.gz"), "wb").write(gzip.compress(mif))
open(os.path.join(d, "manual.mif"), "w").write("<MIFFile 7.00> # FrameMaker interchange\n<Document >\n")
nrrd = b"NRRD0004\ntype: uchar\ndimension: 3\nsizes: 2 2 2\nencoding: raw\n\n" + bytes(8)
open(os.path.join(d, "MR-head.nrrd"), "wb").write(nrrd)
open(os.path.join(d, "dwi.nhdr"), "wb").write(nrrd[:-9] + b"data file: dwi.raw\n")
open(os.path.join(d, "head.mha"), "w").write("ObjectType = Image\nNDims = 3\nDimSize = 2 2 2\nElementType = MET_UCHAR\nElementDataFile = LOCAL\n")
open(os.path.join(d, "head.mhd"), "w").write("ObjectType = Image\nNDims = 3\nDimSize = 2 2 2\nElementType = MET_UCHAR\nElementDataFile = head.raw\n")
# DICOM: shared-mime-info's own type, by extension or by the "DICM" magic.
dcm = b"\0" * 128 + b"DICM" + b"\x02\x00\x10\x00UI\x14\x001.2.840.10008.1.2.1\x00"
open(os.path.join(d, "slice.dcm"), "wb").write(dcm)
open(os.path.join(d, "IM0001"), "wb").write(dcm)
open(os.path.join(d, "pixel.png"), "wb").write(bytes.fromhex(
    "89504e470d0a1a0a0000000d4948445200000001000000010806000000"
    "1f15c4890000000d49444154789c6360000002000154a24f5d0000000049454e44ae426082"))
EOF

type_of() { gio info -a standard::content-type "$1" | sed -n 's/^ *standard::content-type: //p'; }

failures=0
expect() {
    local file="$1" want="$2" got
    got="$(type_of "$F/$file")"
    if [[ "$got" == "$want" ]]; then
        printf '  ok    %-28s %s\n' "$file" "$got"
    else
        printf '  FAIL  %-28s got %s, want %s\n' "$file" "$got" "$want"
        failures=$((failures + 1))
    fi
}

echo "== baseline (no GNOME NIfTI Preview definitions)"
base_gz="$(type_of "$F/sub-001_T1w.nii.gz")"
base_tar="$(type_of "$F/archive.tar.gz")"
base_txtgz="$(type_of "$F/notes.txt.gz")"
base_framemaker="$(type_of "$F/manual.mif")"
echo "  .nii.gz -> $base_gz, .tar.gz -> $base_tar, .txt.gz -> $base_txtgz"

echo "== installed"
cp "$ROOT/mime/gnome-nifti-preview.xml" "$XDG_DATA_HOME/mime/packages/"
update-mime-database "$XDG_DATA_HOME/mime"
expect "sub-001_T1w.nii" image/x-nifti
expect "sub-001_T1w.nii.gz" image/x-compressed-nifti
expect "with spaces é.nii.gz" image/x-compressed-nifti
expect "UPPER.NII.GZ" image/x-compressed-nifti
expect "fake.nii.gz" image/x-compressed-nifti   # routed by name; the backend reports invalid data
expect "archive.tar.gz" "$base_tar"
expect "notes.txt.gz" "$base_txtgz"
expect "plain.gz" application/gzip
expect "readme.txt" text/plain
expect "pixel.png" image/png
expect "brain.mgh" image/x-mgh
expect "aseg.mgz" image/x-compressed-mgh
expect "orig.mgh.gz" image/x-compressed-mgh
expect "dwi.mif" image/x-mrtrix
expect "fod.mif.gz" image/x-compressed-mrtrix
expect "manual.mif" "$base_framemaker"   # FrameMaker keeps its type
expect "MR-head.nrrd" image/x-nrrd
expect "dwi.nhdr" image/x-nrrd-header
expect "head.mha" image/x-metaimage
expect "head.mhd" image/x-metaimage-header
expect "slice.dcm" application/dicom
expect "IM0001" application/dicom

xm="$(xdg-mime query filetype "$F/sub-001_T1w.nii.gz" 2>/dev/null || true)"
if [[ "$xm" == "image/x-compressed-nifti" ]]; then echo "  ok    xdg-mime query filetype     $xm"
else echo "  FAIL  xdg-mime query filetype: $xm"; failures=$((failures + 1)); fi

# Type hierarchy: still a gzip file for archive managers; the alias resolves.
if command -v gjs >/dev/null; then
    out="$(gjs -c '
        const Gio = imports.gi.Gio;
        print(Gio.content_type_is_a("image/x-compressed-nifti", "application/gzip"));
        print(Gio.content_type_is_a("image/x-nifti", "application/gzip"));
        print(Gio.content_type_get_description("image/x-compressed-nifti"));
        print(Gio.content_type_equals("image/x.nifti", "image/x-nifti"));
        print(["image/x-compressed-mgh", "image/x-compressed-mrtrix"].every(t => Gio.content_type_is_a(t, "application/gzip")));')"
    echo "$out" | sed 's/^/  gjs: /'
    [[ "$(echo "$out" | sed -n 1p)" == "true" && "$(echo "$out" | sed -n 2p)" == "false" && "$(echo "$out" | sed -n 4p)" == "true" &&
       "$(echo "$out" | sed -n 5p)" == "true" ]] \
        || { echo "  FAIL  type hierarchy"; failures=$((failures + 1)); }
fi

echo "== removed"
rm "$XDG_DATA_HOME/mime/packages/gnome-nifti-preview.xml"
update-mime-database "$XDG_DATA_HOME/mime"
expect "sub-001_T1w.nii.gz" "$base_gz"
expect "archive.tar.gz" "$base_tar"
expect "plain.gz" application/gzip
got="$(type_of "$F/sub-001_T1w.nii")"
[[ "$got" != image/x-nifti ]] && echo "  ok    sub-001_T1w.nii              $got" || { echo "  FAIL .nii still NIfTI"; failures=$((failures + 1)); }

if (( failures )); then echo "$failures MIME check(s) failed"; exit 1; fi
echo "all MIME checks passed"
