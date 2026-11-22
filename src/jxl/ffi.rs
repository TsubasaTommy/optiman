//! Hand-written bindings for the parts of libjxl 0.13 (BSD-3-Clause) that we use:
//! the encoder, JPEG reconstruction in the decoder, and the thread-pool runner.
//! Layouts follow `third_party/libjxl/lib/include/jxl/*.h`.

#![allow(non_camel_case_types, non_snake_case)]

use std::ffi::c_void;

pub type JXL_BOOL = i32;
pub const JXL_TRUE: JXL_BOOL = 1;
pub const JXL_FALSE: JXL_BOOL = 0;

#[repr(C)]
pub struct JxlEncoder {
    _opaque: [u8; 0],
}
#[repr(C)]
pub struct JxlEncoderFrameSettings {
    _opaque: [u8; 0],
}
#[repr(C)]
pub struct JxlDecoder {
    _opaque: [u8; 0],
}

// JxlEncoderStatus
pub const JXL_ENC_SUCCESS: i32 = 0;
pub const JXL_ENC_NEED_MORE_OUTPUT: i32 = 2;
// JxlEncoderError
pub const JXL_ENC_ERR_OOM: i32 = 2;
pub const JXL_ENC_ERR_JBRD: i32 = 3;
pub const JXL_ENC_ERR_BAD_INPUT: i32 = 4;
pub const JXL_ENC_ERR_NOT_SUPPORTED: i32 = 0x80;
// JxlEncoderFrameSettingId
pub const JXL_ENC_FRAME_SETTING_EFFORT: i32 = 0;

// JxlDecoderStatus
pub const JXL_DEC_SUCCESS: i32 = 0;
pub const JXL_DEC_NEED_IMAGE_OUT_BUFFER: i32 = 5;
pub const JXL_DEC_JPEG_NEED_MORE_OUTPUT: i32 = 6;
pub const JXL_DEC_FULL_IMAGE: i32 = 0x1000;
pub const JXL_DEC_JPEG_RECONSTRUCTION: i32 = 0x2000;

// JxlDataType / JxlEndianness
pub const JXL_TYPE_UINT8: i32 = 2;
pub const JXL_TYPE_UINT16: i32 = 3;
pub const JXL_NATIVE_ENDIAN: i32 = 0;

// Colour encoding enums
pub const JXL_PRIMARIES_P3: i32 = 11;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct JxlPreviewHeader {
    pub xsize: u32,
    pub ysize: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct JxlAnimationHeader {
    pub tps_numerator: u32,
    pub tps_denominator: u32,
    pub num_loops: u32,
    pub have_timecodes: JXL_BOOL,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct JxlBasicInfo {
    pub have_container: JXL_BOOL,
    pub xsize: u32,
    pub ysize: u32,
    pub bits_per_sample: u32,
    pub exponent_bits_per_sample: u32,
    pub intensity_target: f32,
    pub min_nits: f32,
    pub relative_to_max_display: JXL_BOOL,
    pub linear_below: f32,
    pub uses_original_profile: JXL_BOOL,
    pub have_preview: JXL_BOOL,
    pub have_animation: JXL_BOOL,
    pub orientation: i32,
    pub num_color_channels: u32,
    pub num_extra_channels: u32,
    pub alpha_bits: u32,
    pub alpha_exponent_bits: u32,
    pub alpha_premultiplied: JXL_BOOL,
    pub preview: JxlPreviewHeader,
    pub animation: JxlAnimationHeader,
    pub intrinsic_xsize: u32,
    pub intrinsic_ysize: u32,
    pub padding: [u8; 100],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct JxlPixelFormat {
    pub num_channels: u32,
    pub data_type: i32,
    pub endianness: i32,
    pub align: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct JxlColorEncoding {
    pub color_space: i32,
    pub white_point: i32,
    pub white_point_xy: [f64; 2],
    pub primaries: i32,
    pub primaries_red_xy: [f64; 2],
    pub primaries_green_xy: [f64; 2],
    pub primaries_blue_xy: [f64; 2],
    pub transfer_function: i32,
    pub gamma: f64,
    pub rendering_intent: i32,
}

pub type JxlParallelRunner = unsafe extern "C" fn(
    runner_opaque: *mut c_void,
    jpegxl_opaque: *mut c_void,
    init: *const c_void,
    func: *const c_void,
    start_range: u32,
    end_range: u32,
) -> i32;

unsafe extern "C" {
    pub fn JxlEncoderVersion() -> u32;
    pub fn JxlEncoderCreate(memory_manager: *const c_void) -> *mut JxlEncoder;
    pub fn JxlEncoderDestroy(enc: *mut JxlEncoder);
    pub fn JxlEncoderSetParallelRunner(enc: *mut JxlEncoder, runner: JxlParallelRunner, opaque: *mut c_void) -> i32;
    pub fn JxlEncoderGetError(enc: *mut JxlEncoder) -> i32;
    pub fn JxlEncoderProcessOutput(enc: *mut JxlEncoder, next_out: *mut *mut u8, avail_out: *mut usize) -> i32;
    pub fn JxlEncoderUseContainer(enc: *mut JxlEncoder, use_container: JXL_BOOL) -> i32;
    pub fn JxlEncoderStoreJPEGMetadata(enc: *mut JxlEncoder, store: JXL_BOOL) -> i32;
    pub fn JxlEncoderUseBoxes(enc: *mut JxlEncoder) -> i32;
    pub fn JxlEncoderAddBox(
        enc: *mut JxlEncoder,
        box_type: *const u8,
        contents: *const u8,
        size: usize,
        compress_box: JXL_BOOL,
    ) -> i32;
    pub fn JxlEncoderCloseBoxes(enc: *mut JxlEncoder);
    pub fn JxlEncoderCloseInput(enc: *mut JxlEncoder);
    pub fn JxlEncoderInitBasicInfo(info: *mut JxlBasicInfo);
    pub fn JxlEncoderSetBasicInfo(enc: *mut JxlEncoder, info: *const JxlBasicInfo) -> i32;
    pub fn JxlEncoderSetColorEncoding(enc: *mut JxlEncoder, color: *const JxlColorEncoding) -> i32;
    pub fn JxlEncoderSetICCProfile(enc: *mut JxlEncoder, icc: *const u8, size: usize) -> i32;
    pub fn JxlColorEncodingSetToSRGB(color: *mut JxlColorEncoding, is_gray: JXL_BOOL);
    pub fn JxlEncoderFrameSettingsCreate(
        enc: *mut JxlEncoder,
        source: *const JxlEncoderFrameSettings,
    ) -> *mut JxlEncoderFrameSettings;
    pub fn JxlEncoderFrameSettingsSetOption(settings: *mut JxlEncoderFrameSettings, option: i32, value: i64) -> i32;
    pub fn JxlEncoderSetFrameLossless(settings: *mut JxlEncoderFrameSettings, lossless: JXL_BOOL) -> i32;
    pub fn JxlEncoderSetFrameDistance(settings: *mut JxlEncoderFrameSettings, distance: f32) -> i32;
    pub fn JxlEncoderSetExtraChannelDistance(
        settings: *mut JxlEncoderFrameSettings,
        index: usize,
        distance: f32,
    ) -> i32;
    pub fn JxlEncoderDistanceFromQuality(quality: f32) -> f32;
    pub fn JxlEncoderAddJPEGFrame(settings: *const JxlEncoderFrameSettings, buffer: *const u8, size: usize) -> i32;
    pub fn JxlEncoderAddImageFrame(
        settings: *const JxlEncoderFrameSettings,
        format: *const JxlPixelFormat,
        buffer: *const c_void,
        size: usize,
    ) -> i32;

    pub fn JxlDecoderCreate(memory_manager: *const c_void) -> *mut JxlDecoder;
    pub fn JxlDecoderDestroy(dec: *mut JxlDecoder);
    pub fn JxlDecoderSubscribeEvents(dec: *mut JxlDecoder, events_wanted: i32) -> i32;
    pub fn JxlDecoderSetInput(dec: *mut JxlDecoder, data: *const u8, size: usize) -> i32;
    pub fn JxlDecoderCloseInput(dec: *mut JxlDecoder);
    pub fn JxlDecoderProcessInput(dec: *mut JxlDecoder) -> i32;
    pub fn JxlDecoderSetJPEGBuffer(dec: *mut JxlDecoder, data: *mut u8, size: usize) -> i32;
    pub fn JxlDecoderReleaseJPEGBuffer(dec: *mut JxlDecoder) -> usize;

    pub fn JxlThreadParallelRunner(
        runner_opaque: *mut c_void,
        jpegxl_opaque: *mut c_void,
        init: *const c_void,
        func: *const c_void,
        start_range: u32,
        end_range: u32,
    ) -> i32;
    pub fn JxlThreadParallelRunnerCreate(memory_manager: *const c_void, num_worker_threads: usize) -> *mut c_void;
    pub fn JxlThreadParallelRunnerDestroy(runner_opaque: *mut c_void);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_sizes_match_the_c_headers() {
        // Field-by-field sums of the C layouts (with natural alignment) in codestream_header.h,
        // types.h and color_encoding.h.
        assert_eq!(size_of::<JxlBasicInfo>(), 204);
        assert_eq!(size_of::<JxlPixelFormat>(), 24);
        assert_eq!(size_of::<JxlColorEncoding>(), 104);
    }
}
