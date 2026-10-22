import numpy as np
import pytest

from nifti_preview_ref import synthetic


@pytest.fixture
def marker():
    return synthetic.marker_phantom()


@pytest.fixture
def write(tmp_path):
    """write(name, data, affine, **kwargs) -> path of a NIfTI file in tmp_path."""

    def _write(name, data, affine, **kwargs):
        return str(synthetic.save(tmp_path / name, data, affine, **kwargs))

    return _write


@pytest.fixture
def t1_path(write):
    data, affine = synthetic.head_phantom((48, 56, 44))
    return write("sub-001_T1w.nii.gz", data, affine, dtype=np.int16)


@pytest.fixture
def labels_path(write):
    data, affine = synthetic.label_phantom()
    return write("sub-001_dseg.nii.gz", data, affine, dtype=np.int16)
