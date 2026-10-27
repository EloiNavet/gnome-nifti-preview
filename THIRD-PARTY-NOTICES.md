# Third-party notices

GNOME NIfTI Preview is GPL-2.0-or-later. It includes or derives from the
following works.

## FreeSurfer label colours and names (`backend/src/freesurfer_lut.tsv`)

The colours and names of the FreeSurfer labels (aseg, Desikan-Killiany and
Destrieux cortical parcellations, wmparc) come from FreeSurferColorLUT.txt
(version 1.105) of [FreeSurfer](https://surfer.nmr.mgh.harvard.edu/),
Laboratory for Computational Neuroimaging, Athinoula A. Martinos Center for
Biomedical Imaging. They were taken from the table of MIQ (below); the rule
that recognises a FreeSurfer parcellation is MIQ's as well.

## MIQ

[MIQ](https://github.com/marcoduering/MIQ) by Marco Duering, which inspired
this project, under the MIT licence:

```
MIT License

Copyright (c) 2026 Marco Duering

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

The colours of intensities `inferno` and `viridis` are sampled from
[matplotlib](https://matplotlib.org/)'s colormaps of the same names (CC0).

## OpenJPEG (compiled into the backend)

The backend decodes JPEG 2000 and High-Throughput JPEG 2000 DICOM images with
[OpenJPEG](https://www.openjpeg.org/) 2.5, compiled from the sources vendored
by the `openjpeg-sys` crate, under the 2-clause BSD licence:

```
Copyright (c) 2002-2014, Universite catholique de Louvain (UCL), Belgium
Copyright (c) 2002-2014, Professor Benoit Macq
Copyright (c) 2003-2014, Antonin Descampe
Copyright (c) 2003-2009, Francois-Olivier Devaux
Copyright (c) 2005, Herve Drolon, FreeImage Team
Copyright (c) 2002-2003, Yannick Verschueren
Copyright (c) 2001-2003, David Janssens
Copyright (c) 2011-2012, Centre National d'Etudes Spatiales (CNES), France
Copyright (c) 2012, CS Systemes d'Information, France
Copyright (c) 2021, Aous Naman
Copyright (c) 2021, Kakadu Software Pty Ltd, Australia
Copyright (c) 2021, The University of New South Wales, Australia
All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions
are met:
1. Redistributions of source code must retain the above copyright
   notice, this list of conditions and the following disclaimer.
2. Redistributions in binary form must reproduce the above copyright
   notice, this list of conditions and the following disclaimer in the
   documentation and/or other materials provided with the distribution.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS `AS IS'
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
ARE DISCLAIMED.  IN NO EVENT SHALL THE COPYRIGHT OWNER OR CONTRIBUTORS BE
LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
POSSIBILITY OF SUCH DAMAGE.
```

The other libraries compiled into the backend are Rust crates and libdeflate,
under the MIT, Apache-2.0 or Zlib licences (`backend/Cargo.lock` lists them;
`cargo deny check` verifies the licences).

The JPEG, JPEG-LS, RLE and bzip2 decoders of the backend are written for this
project; the JPEG-LS decoder follows ITU-T T.87 as [CharLS](https://github.com/team-charls/charls)
implements it. The small codec streams of `backend/src/formats/dicom/testdata/`
were written for these tests by CharLS, OpenJPEG and libjpeg (through pyjpegls,
pylibjpeg-openjpeg and Pillow) from synthetic images (two of them, `overflow.jls` and
`empty_component.jpg`, are damaged copies the fuzzer made of those); the bzip2 stream of
`backend/src/formats/testdata/` by the bzip2 tool, from a line of text.
