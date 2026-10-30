use windows::Win32::Media::MediaFoundation::{
    IMFActivate, MF_VERSION, MFMediaType_Video, MFSTARTUP_LITE, MFShutdown, MFStartup,
    MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
    MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_H264, MFVideoFormat_HEVC,
};
use windows::Win32::System::Com::{
    COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};

#[derive(Clone, Copy)]
pub enum MfCodec {
    H264,
    Hevc,
}

/// Whether Media Foundation has a hardware encoder for `codec` on this machine.
pub fn has_hardware_encoder(codec: MfCodec) -> bool {
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: match codec {
            MfCodec::H264 => MFVideoFormat_H264,
            MfCodec::Hevc => MFVideoFormat_HEVC,
        },
    };
    // SAFETY: COM and Media Foundation are started and stopped in balanced pairs on this
    // thread; `activates` holds `count` interfaces that are released before it is freed.
    unsafe {
        let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let found = MFStartup(MF_VERSION, MFSTARTUP_LITE).is_ok() && {
            let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut count = 0u32;
            let listed = MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                None,
                Some(&output),
                &mut activates,
                &mut count,
            )
            .is_ok();
            if !activates.is_null() {
                for index in 0..count as usize {
                    std::ptr::drop_in_place(activates.add(index));
                }
                CoTaskMemFree(Some(activates.cast()));
            }
            MFShutdown().ok();
            listed && count > 0
        };
        if com {
            CoUninitialize();
        }
        found
    }
}
