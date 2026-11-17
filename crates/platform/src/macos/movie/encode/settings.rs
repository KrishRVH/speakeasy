use anyhow::Context as _;
use objc2::{rc::Retained, runtime::AnyObject};
use objc2_av_foundation::{
    AVVideoAllowFrameReorderingKey, AVVideoAverageBitRateKey, AVVideoCodecKey,
    AVVideoCodecTypeH264, AVVideoColorPrimaries_ITU_R_709_2, AVVideoColorPrimariesKey,
    AVVideoColorPropertiesKey, AVVideoCompressionPropertiesKey, AVVideoExpectedSourceFrameRateKey,
    AVVideoHeightKey, AVVideoMaxKeyFrameIntervalKey, AVVideoProfileLevelH264HighAutoLevel,
    AVVideoProfileLevelKey, AVVideoTransferFunction_IEC_sRGB, AVVideoTransferFunctionKey,
    AVVideoWidthKey, AVVideoYCbCrMatrix_ITU_R_709_2, AVVideoYCbCrMatrixKey,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString};

pub(super) fn video(
    [width, height]: [u32; 2],
) -> anyhow::Result<Retained<NSDictionary<NSString, AnyObject>>> {
    let setting =
        |value: Option<&'static NSString>| value.context("Native video setting is unavailable");
    let width = NSNumber::new_u32(width);
    let height = NSNumber::new_u32(height);
    let bitrate = NSNumber::new_u32(8_000_000);
    let rate = NSNumber::new_u32(30);
    let reorder = NSNumber::new_bool(false);

    unsafe {
        let color = NSDictionary::from_slices(
            &[
                setting(AVVideoColorPrimariesKey)?,
                setting(AVVideoTransferFunctionKey)?,
                setting(AVVideoYCbCrMatrixKey)?,
            ],
            &[
                setting(AVVideoColorPrimaries_ITU_R_709_2)?,
                setting(AVVideoTransferFunction_IEC_sRGB)?,
                setting(AVVideoYCbCrMatrix_ITU_R_709_2)?,
            ],
        );
        let compression = NSDictionary::from_slices(
            &[
                setting(AVVideoAverageBitRateKey)?,
                setting(AVVideoExpectedSourceFrameRateKey)?,
                setting(AVVideoMaxKeyFrameIntervalKey)?,
                setting(AVVideoAllowFrameReorderingKey)?,
                setting(AVVideoProfileLevelKey)?,
            ],
            &[
                &*bitrate as &AnyObject,
                &*rate,
                &*rate,
                &*reorder,
                setting(AVVideoProfileLevelH264HighAutoLevel)?,
            ],
        );
        Ok(NSDictionary::from_slices(
            &[
                setting(AVVideoCodecKey)?,
                setting(AVVideoWidthKey)?,
                setting(AVVideoHeightKey)?,
                setting(AVVideoColorPropertiesKey)?,
                setting(AVVideoCompressionPropertiesKey)?,
            ],
            &[
                setting(AVVideoCodecTypeH264)? as &AnyObject,
                &*width,
                &*height,
                &*color,
                &*compression,
            ],
        ))
    }
}
