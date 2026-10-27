//! The DICOM attributes this reader uses, and their value representation
//! (needed for files in implicit VR, and for values stored as UN).

pub type Tag = u32;

pub const fn tag(group: u16, element: u16) -> Tag {
    ((group as u32) << 16) | element as u32
}

pub const ITEM: Tag = tag(0xFFFE, 0xE000);
pub const ITEM_DELIMITATION: Tag = tag(0xFFFE, 0xE00D);
pub const SEQUENCE_DELIMITATION: Tag = tag(0xFFFE, 0xE0DD);

// File meta information.
pub const MEDIA_STORAGE_SOP_CLASS: Tag = tag(0x0002, 0x0002);
pub const TRANSFER_SYNTAX: Tag = tag(0x0002, 0x0010);

// Directory (DICOMDIR).
pub const DIRECTORY_RECORDS: Tag = tag(0x0004, 0x1220);
pub const NEXT_RECORD: Tag = tag(0x0004, 0x1400);
pub const LOWER_RECORD: Tag = tag(0x0004, 0x1420);
pub const RECORD_TYPE: Tag = tag(0x0004, 0x1430);
pub const REFERENCED_FILE_ID: Tag = tag(0x0004, 0x1500);

pub const SPECIFIC_CHARACTER_SET: Tag = tag(0x0008, 0x0005);
pub const IMAGE_TYPE: Tag = tag(0x0008, 0x0008);
pub const SOP_CLASS: Tag = tag(0x0008, 0x0016);
pub const SOP_INSTANCE: Tag = tag(0x0008, 0x0018);
pub const STUDY_DATE: Tag = tag(0x0008, 0x0020);
pub const SERIES_DATE: Tag = tag(0x0008, 0x0021);
pub const ACQUISITION_DATE: Tag = tag(0x0008, 0x0022);
pub const STUDY_TIME: Tag = tag(0x0008, 0x0030);
pub const SERIES_TIME: Tag = tag(0x0008, 0x0031);
pub const ACQUISITION_TIME: Tag = tag(0x0008, 0x0032);
pub const ACCESSION_NUMBER: Tag = tag(0x0008, 0x0050);
pub const MODALITY: Tag = tag(0x0008, 0x0060);
pub const MANUFACTURER: Tag = tag(0x0008, 0x0070);
pub const INSTITUTION: Tag = tag(0x0008, 0x0080);
pub const REFERRING_PHYSICIAN: Tag = tag(0x0008, 0x0090);
pub const STUDY_DESCRIPTION: Tag = tag(0x0008, 0x1030);
pub const SERIES_DESCRIPTION: Tag = tag(0x0008, 0x103E);
pub const MODEL: Tag = tag(0x0008, 0x1090);
pub const CONTENT_DATE: Tag = tag(0x0008, 0x0023);
pub const CODE_VALUE: Tag = tag(0x0008, 0x0100);
pub const CODE_MEANING: Tag = tag(0x0008, 0x0104);
pub const DOCUMENT_TITLE: Tag = tag(0x0042, 0x0010);

pub const PATIENT_NAME: Tag = tag(0x0010, 0x0010);
pub const PATIENT_ID: Tag = tag(0x0010, 0x0020);
pub const PATIENT_BIRTH_DATE: Tag = tag(0x0010, 0x0030);
pub const PATIENT_SEX: Tag = tag(0x0010, 0x0040);
pub const PATIENT_AGE: Tag = tag(0x0010, 0x1010);

pub const BODY_PART: Tag = tag(0x0018, 0x0015);
pub const SCANNING_SEQUENCE: Tag = tag(0x0018, 0x0020);
pub const SEQUENCE_VARIANT: Tag = tag(0x0018, 0x0021);
pub const SEQUENCE_NAME: Tag = tag(0x0018, 0x0024);
pub const SLICE_THICKNESS: Tag = tag(0x0018, 0x0050);
pub const KVP: Tag = tag(0x0018, 0x0060);
pub const REPETITION_TIME: Tag = tag(0x0018, 0x0080);
pub const ECHO_TIME: Tag = tag(0x0018, 0x0081);
pub const INVERSION_TIME: Tag = tag(0x0018, 0x0082);
pub const ECHO_NUMBERS: Tag = tag(0x0018, 0x0086);
pub const FIELD_STRENGTH: Tag = tag(0x0018, 0x0087);
pub const SPACING_BETWEEN_SLICES: Tag = tag(0x0018, 0x0088);
pub const PROTOCOL_NAME: Tag = tag(0x0018, 0x1030);
pub const FRAME_TIME: Tag = tag(0x0018, 0x1063);
pub const TRIGGER_TIME: Tag = tag(0x0018, 0x1060);
pub const EXPOSURE: Tag = tag(0x0018, 0x1152);
pub const IMAGER_PIXEL_SPACING: Tag = tag(0x0018, 0x1164);
pub const CONVOLUTION_KERNEL: Tag = tag(0x0018, 0x1210);
pub const FLIP_ANGLE: Tag = tag(0x0018, 0x1314);
pub const EFFECTIVE_ECHO_TIME: Tag = tag(0x0018, 0x9082);
pub const MR_TIMING_SEQUENCE: Tag = tag(0x0018, 0x9112);
pub const MR_ECHO_SEQUENCE: Tag = tag(0x0018, 0x9114);
pub const MR_DIFFUSION_SEQUENCE: Tag = tag(0x0018, 0x9117);
pub const DIFFUSION_B_VALUE: Tag = tag(0x0018, 0x9087);

pub const STUDY_INSTANCE: Tag = tag(0x0020, 0x000D);
pub const SERIES_INSTANCE: Tag = tag(0x0020, 0x000E);
pub const SERIES_NUMBER: Tag = tag(0x0020, 0x0011);
pub const ACQUISITION_NUMBER: Tag = tag(0x0020, 0x0012);
pub const INSTANCE_NUMBER: Tag = tag(0x0020, 0x0013);
pub const IMAGE_POSITION: Tag = tag(0x0020, 0x0032);
pub const IMAGE_ORIENTATION: Tag = tag(0x0020, 0x0037);
pub const TEMPORAL_POSITION_IDENTIFIER: Tag = tag(0x0020, 0x0100);
pub const SLICE_LOCATION: Tag = tag(0x0020, 0x1041);
pub const STACK_ID: Tag = tag(0x0020, 0x9056);
pub const IN_STACK_POSITION: Tag = tag(0x0020, 0x9057);
pub const FRAME_CONTENT_SEQUENCE: Tag = tag(0x0020, 0x9111);
pub const PLANE_POSITION_SEQUENCE: Tag = tag(0x0020, 0x9113);
pub const PLANE_ORIENTATION_SEQUENCE: Tag = tag(0x0020, 0x9116);
pub const TEMPORAL_POSITION_INDEX: Tag = tag(0x0020, 0x9128);
pub const DIMENSION_INDEX_VALUES: Tag = tag(0x0020, 0x9157);
pub const DIMENSION_INDEX_SEQUENCE: Tag = tag(0x0020, 0x9222);
pub const DIMENSION_INDEX_POINTER: Tag = tag(0x0020, 0x9165);

pub const SAMPLES_PER_PIXEL: Tag = tag(0x0028, 0x0002);
pub const PHOTOMETRIC: Tag = tag(0x0028, 0x0004);
pub const PLANAR_CONFIGURATION: Tag = tag(0x0028, 0x0006);
pub const NUMBER_OF_FRAMES: Tag = tag(0x0028, 0x0008);
pub const ROWS: Tag = tag(0x0028, 0x0010);
pub const COLUMNS: Tag = tag(0x0028, 0x0011);
pub const PIXEL_SPACING: Tag = tag(0x0028, 0x0030);
pub const BITS_ALLOCATED: Tag = tag(0x0028, 0x0100);
pub const BITS_STORED: Tag = tag(0x0028, 0x0101);
pub const HIGH_BIT: Tag = tag(0x0028, 0x0102);
pub const PIXEL_REPRESENTATION: Tag = tag(0x0028, 0x0103);
pub const WINDOW_CENTER: Tag = tag(0x0028, 0x1050);
pub const WINDOW_WIDTH: Tag = tag(0x0028, 0x1051);
pub const RESCALE_INTERCEPT: Tag = tag(0x0028, 0x1052);
pub const RESCALE_SLOPE: Tag = tag(0x0028, 0x1053);
pub const RESCALE_TYPE: Tag = tag(0x0028, 0x1054);
pub const VOI_LUT_FUNCTION: Tag = tag(0x0028, 0x1056);
pub const RED_DESCRIPTOR: Tag = tag(0x0028, 0x1101);
pub const GREEN_DESCRIPTOR: Tag = tag(0x0028, 0x1102);
pub const BLUE_DESCRIPTOR: Tag = tag(0x0028, 0x1103);
pub const RED_DATA: Tag = tag(0x0028, 0x1201);
pub const GREEN_DATA: Tag = tag(0x0028, 0x1202);
pub const BLUE_DATA: Tag = tag(0x0028, 0x1203);
pub const SEGMENTED_RED_DATA: Tag = tag(0x0028, 0x1221);
pub const MODALITY_LUT_SEQUENCE: Tag = tag(0x0028, 0x3000);
pub const LUT_DESCRIPTOR: Tag = tag(0x0028, 0x3002);
pub const LUT_DATA: Tag = tag(0x0028, 0x3006);
pub const PIXEL_MEASURES_SEQUENCE: Tag = tag(0x0028, 0x9110);
pub const FRAME_VOI_LUT_SEQUENCE: Tag = tag(0x0028, 0x9132);
pub const PIXEL_VALUE_TRANSFORMATION_SEQUENCE: Tag = tag(0x0028, 0x9145);

// Waveforms.
pub const NUMBER_OF_WAVEFORM_CHANNELS: Tag = tag(0x003A, 0x0005);
pub const NUMBER_OF_WAVEFORM_SAMPLES: Tag = tag(0x003A, 0x0010);
pub const SAMPLING_FREQUENCY: Tag = tag(0x003A, 0x001A);
pub const MULTIPLEX_GROUP_LABEL: Tag = tag(0x003A, 0x0020);
pub const CHANNEL_DEFINITION_SEQUENCE: Tag = tag(0x003A, 0x0200);
pub const CHANNEL_SOURCE_SEQUENCE: Tag = tag(0x003A, 0x0208);
pub const WAVEFORM_SEQUENCE: Tag = tag(0x5400, 0x0100);

// Structured reports.
pub const MEASUREMENT_UNITS_CODE_SEQUENCE: Tag = tag(0x0040, 0x08EA);
pub const RELATIONSHIP_TYPE: Tag = tag(0x0040, 0xA010);
pub const VALUE_TYPE: Tag = tag(0x0040, 0xA040);
pub const CONCEPT_NAME_CODE_SEQUENCE: Tag = tag(0x0040, 0xA043);
pub const DATETIME: Tag = tag(0x0040, 0xA120);
pub const DATE: Tag = tag(0x0040, 0xA121);
pub const TIME: Tag = tag(0x0040, 0xA122);
pub const UID: Tag = tag(0x0040, 0xA124);
pub const TEXT_VALUE: Tag = tag(0x0040, 0xA160);
pub const CONCEPT_CODE_SEQUENCE: Tag = tag(0x0040, 0xA168);
pub const MEASURED_VALUE_SEQUENCE: Tag = tag(0x0040, 0xA300);
pub const NUMERIC_VALUE: Tag = tag(0x0040, 0xA30A);
pub const COMPLETION_FLAG: Tag = tag(0x0040, 0xA491);
pub const VERIFICATION_FLAG: Tag = tag(0x0040, 0xA493);
pub const CONTENT_SEQUENCE: Tag = tag(0x0040, 0xA730);

pub const REFERENCED_SEGMENT_NUMBER: Tag = tag(0x0062, 0x000B);
pub const SEGMENT_IDENTIFICATION_SEQUENCE: Tag = tag(0x0062, 0x000A);

// Radiotherapy structures and plans.
pub const STRUCTURE_SET_LABEL: Tag = tag(0x3006, 0x0002);
pub const STRUCTURE_SET_ROI_SEQUENCE: Tag = tag(0x3006, 0x0020);
pub const ROI_NUMBER: Tag = tag(0x3006, 0x0022);
pub const ROI_NAME: Tag = tag(0x3006, 0x0026);
pub const RT_PLAN_LABEL: Tag = tag(0x300A, 0x0002);
pub const RT_PLAN_NAME: Tag = tag(0x300A, 0x0003);
pub const PLAN_INTENT: Tag = tag(0x300A, 0x000A);
pub const FRACTION_GROUP_SEQUENCE: Tag = tag(0x300A, 0x0070);
pub const NUMBER_OF_FRACTIONS_PLANNED: Tag = tag(0x300A, 0x0078);
pub const BEAM_SEQUENCE: Tag = tag(0x300A, 0x00B0);
pub const BEAM_NAME: Tag = tag(0x300A, 0x00C2);
pub const RADIATION_TYPE: Tag = tag(0x300A, 0x00C6);
pub const ION_BEAM_SEQUENCE: Tag = tag(0x300A, 0x03A2);

pub const GRID_FRAME_OFFSETS: Tag = tag(0x3004, 0x000C);
pub const DOSE_GRID_SCALING: Tag = tag(0x3004, 0x000E);

pub const SHARED_FUNCTIONAL_GROUPS: Tag = tag(0x5200, 0x9229);
pub const PER_FRAME_FUNCTIONAL_GROUPS: Tag = tag(0x5200, 0x9230);

pub const EXTENDED_OFFSET_TABLE: Tag = tag(0x7FE0, 0x0001);
pub const EXTENDED_OFFSET_LENGTHS: Tag = tag(0x7FE0, 0x0002);
pub const FLOAT_PIXEL_DATA: Tag = tag(0x7FE0, 0x0008);
pub const DOUBLE_PIXEL_DATA: Tag = tag(0x7FE0, 0x0009);
pub const PIXEL_DATA: Tag = tag(0x7FE0, 0x0010);

// Siemens private attributes, valid only under their private creator.
pub const SIEMENS_MR_CREATOR: Tag = tag(0x0019, 0x0010);
pub const SIEMENS_MOSAIC_IMAGES: Tag = tag(0x0019, 0x100A);
pub const SIEMENS_CSA_CREATOR: Tag = tag(0x0029, 0x0010);
pub const SIEMENS_CSA_IMAGE: Tag = tag(0x0029, 0x1010);

pub fn is_pixel_data(t: Tag) -> bool {
    matches!(t, PIXEL_DATA | FLOAT_PIXEL_DATA | DOUBLE_PIXEL_DATA)
}

/// The value representation of the attributes read here (implicit VR files
/// do not carry it); "UN" for the others.
pub fn vr_of(t: Tag) -> [u8; 2] {
    let vr: &[u8; 2] = match t {
        MEDIA_STORAGE_SOP_CLASS | TRANSFER_SYNTAX | SOP_CLASS | SOP_INSTANCE | STUDY_INSTANCE | SERIES_INSTANCE => b"UI",
        DIRECTORY_RECORDS | MR_TIMING_SEQUENCE | MR_ECHO_SEQUENCE | MR_DIFFUSION_SEQUENCE | FRAME_CONTENT_SEQUENCE
        | PLANE_POSITION_SEQUENCE | PLANE_ORIENTATION_SEQUENCE | DIMENSION_INDEX_SEQUENCE | MODALITY_LUT_SEQUENCE
        | PIXEL_MEASURES_SEQUENCE | FRAME_VOI_LUT_SEQUENCE | PIXEL_VALUE_TRANSFORMATION_SEQUENCE
        | SEGMENT_IDENTIFICATION_SEQUENCE | SHARED_FUNCTIONAL_GROUPS | PER_FRAME_FUNCTIONAL_GROUPS
        | CHANNEL_DEFINITION_SEQUENCE | CHANNEL_SOURCE_SEQUENCE | WAVEFORM_SEQUENCE | MEASUREMENT_UNITS_CODE_SEQUENCE
        | CONCEPT_NAME_CODE_SEQUENCE | CONCEPT_CODE_SEQUENCE | MEASURED_VALUE_SEQUENCE | CONTENT_SEQUENCE
        | STRUCTURE_SET_ROI_SEQUENCE | FRACTION_GROUP_SEQUENCE | BEAM_SEQUENCE | ION_BEAM_SEQUENCE => b"SQ",
        NEXT_RECORD | LOWER_RECORD | TEMPORAL_POSITION_INDEX | IN_STACK_POSITION | DIMENSION_INDEX_VALUES
        | NUMBER_OF_WAVEFORM_SAMPLES => b"UL",
        RECORD_TYPE | SPECIFIC_CHARACTER_SET | IMAGE_TYPE | MODALITY | PATIENT_SEX | BODY_PART | SCANNING_SEQUENCE
        | SEQUENCE_VARIANT | PHOTOMETRIC | RESCALE_TYPE | VOI_LUT_FUNCTION | REFERENCED_FILE_ID | RELATIONSHIP_TYPE
        | VALUE_TYPE | COMPLETION_FLAG | VERIFICATION_FLAG | PLAN_INTENT | RADIATION_TYPE => b"CS",
        STUDY_DATE | SERIES_DATE | ACQUISITION_DATE | PATIENT_BIRTH_DATE | CONTENT_DATE | DATE => b"DA",
        STUDY_TIME | SERIES_TIME | ACQUISITION_TIME | TIME => b"TM",
        DATETIME => b"DT",
        UID => b"UI",
        TEXT_VALUE => b"UT",
        ACCESSION_NUMBER | SEQUENCE_NAME | CONVOLUTION_KERNEL | STACK_ID | CODE_VALUE | MULTIPLEX_GROUP_LABEL
        | STRUCTURE_SET_LABEL | RT_PLAN_LABEL => b"SH",
        MANUFACTURER | INSTITUTION | STUDY_DESCRIPTION | SERIES_DESCRIPTION | MODEL | PATIENT_ID | PROTOCOL_NAME
        | SIEMENS_MR_CREATOR | SIEMENS_CSA_CREATOR | CODE_MEANING | ROI_NAME | RT_PLAN_NAME | BEAM_NAME => b"LO",
        DOCUMENT_TITLE => b"ST",
        REFERRING_PHYSICIAN | PATIENT_NAME => b"PN",
        PATIENT_AGE => b"AS",
        SLICE_THICKNESS | KVP | REPETITION_TIME | ECHO_TIME | INVERSION_TIME | FIELD_STRENGTH | SPACING_BETWEEN_SLICES
        | FRAME_TIME | TRIGGER_TIME | IMAGER_PIXEL_SPACING | FLIP_ANGLE | IMAGE_POSITION | IMAGE_ORIENTATION
        | SLICE_LOCATION | PIXEL_SPACING | WINDOW_CENTER | WINDOW_WIDTH | RESCALE_INTERCEPT | RESCALE_SLOPE
        | GRID_FRAME_OFFSETS | DOSE_GRID_SCALING | SAMPLING_FREQUENCY | NUMERIC_VALUE => b"DS",
        ECHO_NUMBERS | SERIES_NUMBER | ACQUISITION_NUMBER | INSTANCE_NUMBER | TEMPORAL_POSITION_IDENTIFIER
        | NUMBER_OF_FRAMES | EXPOSURE | ROI_NUMBER | NUMBER_OF_FRACTIONS_PLANNED => b"IS",
        EFFECTIVE_ECHO_TIME | DIFFUSION_B_VALUE => b"FD",
        DIMENSION_INDEX_POINTER => b"AT",
        SAMPLES_PER_PIXEL | PLANAR_CONFIGURATION | ROWS | COLUMNS | BITS_ALLOCATED | BITS_STORED | HIGH_BIT
        | PIXEL_REPRESENTATION | REFERENCED_SEGMENT_NUMBER | SIEMENS_MOSAIC_IMAGES | NUMBER_OF_WAVEFORM_CHANNELS => b"US",
        // Palette descriptors are US or SS: the values used are read as unsigned.
        RED_DESCRIPTOR | GREEN_DESCRIPTOR | BLUE_DESCRIPTOR | LUT_DESCRIPTOR => b"US",
        RED_DATA | GREEN_DATA | BLUE_DATA | SEGMENTED_RED_DATA | LUT_DATA | PIXEL_DATA => b"OW",
        SIEMENS_CSA_IMAGE => b"OB",
        FLOAT_PIXEL_DATA => b"OF",
        DOUBLE_PIXEL_DATA => b"OD",
        EXTENDED_OFFSET_TABLE | EXTENDED_OFFSET_LENGTHS => b"OV",
        _ => b"UN",
    };
    *vr
}

/// Explicit VRs whose length takes 4 bytes (after 2 reserved bytes).
pub fn long_length(vr: [u8; 2]) -> bool {
    matches!(&vr, b"OB" | b"OD" | b"OF" | b"OL" | b"OV" | b"OW" | b"SQ" | b"UC" | b"UN" | b"UR" | b"UT" | b"SV" | b"UV")
}

pub fn is_vr(b: [u8; 2]) -> bool {
    b[0].is_ascii_uppercase() && b[1].is_ascii_uppercase()
}
