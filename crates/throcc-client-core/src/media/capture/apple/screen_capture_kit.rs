#![allow(
    clippy::missing_safety_doc,
    reason = "extern_protocol! makes unsafe traits that carry no docs"
)]

use block2::Block;
use dispatch2::DispatchQueue;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{extern_class, extern_methods, extern_protocol};
use objc2_core_foundation::CGRect;
#[cfg(target_os = "macos")]
use objc2_core_media::CMTime;
#[cfg(all(test, target_os = "macos"))]
use objc2_foundation::NSArray;
use objc2_foundation::NSError;

// iOS links ScreenCaptureKit weakly through the app's own linker flags, since iOS 26 has
// none.
#[cfg(target_os = "macos")]
#[link(name = "ScreenCaptureKit", kind = "framework")]
unsafe extern "C" {}

/// `SCStreamOutputType`.
pub const OUTPUT_SCREEN: isize = 0;
pub const OUTPUT_AUDIO: isize = 1;

/// `SCContentSharingPickerMode`.
#[cfg(target_os = "macos")]
pub mod picker_mode {
    pub const SINGLE_WINDOW: usize = 1 << 0;
    pub const SINGLE_APPLICATION: usize = 1 << 2;
    pub const SINGLE_DISPLAY: usize = 1 << 4;
}

extern_class!(
    #[unsafe(super(NSObject))]
    #[derive(Debug, PartialEq, Eq, Hash)]
    pub struct SCContentFilter;
);

// SAFETY: a content filter is never changed once made, and ScreenCaptureKit hands it from
// the picker's thread to whichever thread starts the stream.
unsafe impl Send for SCContentFilter {}
// SAFETY: as above, it is only read.
unsafe impl Sync for SCContentFilter {}

impl SCContentFilter {
    extern_methods!(
        #[unsafe(method(contentRect))]
        pub fn content_rect(&self) -> CGRect;

        #[unsafe(method(pointPixelScale))]
        pub fn point_pixel_scale(&self) -> f32;

        #[cfg(all(test, target_os = "macos"))]
        #[unsafe(method(initWithDisplay:excludingWindows:))]
        pub fn init_with_display(
            this: Allocated<Self>,
            display: &SCDisplay,
            excluded: &NSArray<NSObject>,
        ) -> Retained<Self>;
    );
}

extern_class!(
    #[unsafe(super(NSObject))]
    #[derive(Debug, PartialEq, Eq, Hash)]
    pub struct SCStreamConfiguration;
);

impl SCStreamConfiguration {
    extern_methods!(
        #[unsafe(method(new))]
        pub fn new() -> Retained<Self>;

        #[unsafe(method(setWidth:))]
        pub fn set_width(&self, width: usize);

        #[unsafe(method(setHeight:))]
        pub fn set_height(&self, height: usize);

        #[unsafe(method(setCapturesAudio:))]
        pub fn set_captures_audio(&self, captures: bool);

        #[unsafe(method(setSampleRate:))]
        pub fn set_sample_rate(&self, rate: isize);

        #[unsafe(method(setChannelCount:))]
        pub fn set_channel_count(&self, count: isize);

        #[unsafe(method(setExcludesCurrentProcessAudio:))]
        pub fn set_excludes_current_process_audio(&self, excludes: bool);

        #[cfg(target_os = "macos")]
        #[unsafe(method(setPixelFormat:))]
        pub fn set_pixel_format(&self, format: u32);

        #[cfg(target_os = "macos")]
        #[unsafe(method(setMinimumFrameInterval:))]
        pub fn set_minimum_frame_interval(&self, interval: CMTime);

        #[cfg(target_os = "macos")]
        #[unsafe(method(setQueueDepth:))]
        pub fn set_queue_depth(&self, depth: isize);

        #[cfg(target_os = "macos")]
        #[unsafe(method(setShowsCursor:))]
        pub fn set_shows_cursor(&self, shows: bool);
    );
}

extern_class!(
    #[unsafe(super(NSObject))]
    #[derive(Debug, PartialEq, Eq, Hash)]
    pub struct SCStream;
);

impl SCStream {
    extern_methods!(
        #[unsafe(method(initWithFilter:configuration:delegate:))]
        pub fn init_with_filter(
            this: Allocated<Self>,
            filter: &SCContentFilter,
            configuration: &SCStreamConfiguration,
            delegate: Option<&ProtocolObject<dyn SCStreamDelegate>>,
        ) -> Retained<Self>;

        /// # Safety
        ///
        /// `queue` must be a serial queue.
        #[unsafe(method(addStreamOutput:type:sampleHandlerQueue:error:_))]
        pub unsafe fn add_stream_output(
            &self,
            output: &ProtocolObject<dyn SCStreamOutput>,
            kind: isize,
            queue: Option<&DispatchQueue>,
        ) -> Result<(), Retained<NSError>>;

        #[unsafe(method(startCaptureWithCompletionHandler:))]
        pub fn start_capture(&self, handler: Option<&Block<dyn Fn(*mut NSError)>>);

        #[unsafe(method(stopCaptureWithCompletionHandler:))]
        pub fn stop_capture(&self, handler: Option<&Block<dyn Fn(*mut NSError)>>);
    );
}

#[cfg(target_os = "macos")]
extern_class!(
    #[unsafe(super(NSObject))]
    #[derive(Debug, PartialEq, Eq, Hash)]
    pub struct SCContentSharingPickerConfiguration;
);

#[cfg(target_os = "macos")]
impl SCContentSharingPickerConfiguration {
    extern_methods!(
        #[unsafe(method(new))]
        pub fn new() -> Retained<Self>;

        #[unsafe(method(setAllowedPickerModes:))]
        pub fn set_allowed_picker_modes(&self, modes: usize);
    );
}

extern_class!(
    #[unsafe(super(NSObject))]
    #[derive(Debug, PartialEq, Eq, Hash)]
    pub struct SCContentSharingPicker;
);

impl SCContentSharingPicker {
    extern_methods!(
        #[unsafe(method(sharedPicker))]
        pub fn shared() -> Retained<Self>;

        #[unsafe(method(setActive:))]
        pub fn set_active(&self, active: bool);

        #[unsafe(method(addObserver:))]
        pub fn add_observer(&self, observer: &ProtocolObject<dyn SCContentSharingPickerObserver>);

        #[unsafe(method(removeObserver:))]
        pub fn remove_observer(
            &self,
            observer: &ProtocolObject<dyn SCContentSharingPickerObserver>,
        );

        #[unsafe(method(present))]
        pub fn present(&self);

        #[cfg(target_os = "macos")]
        #[unsafe(method(setDefaultConfiguration:))]
        pub fn set_default_configuration(
            &self,
            configuration: &SCContentSharingPickerConfiguration,
        );
    );
}

extern_protocol!(
    pub unsafe trait SCStreamOutput: NSObjectProtocol {}
);

extern_protocol!(
    pub unsafe trait SCStreamDelegate: NSObjectProtocol {}
);

extern_protocol!(
    pub unsafe trait SCContentSharingPickerObserver: NSObjectProtocol {}
);

#[cfg(all(test, target_os = "macos"))]
extern_class!(
    #[unsafe(super(NSObject))]
    #[derive(Debug, PartialEq, Eq, Hash)]
    pub struct SCDisplay;
);

#[cfg(all(test, target_os = "macos"))]
impl SCDisplay {
    extern_methods!(
        #[unsafe(method(width))]
        pub fn width(&self) -> isize;

        #[unsafe(method(height))]
        pub fn height(&self) -> isize;
    );
}

#[cfg(all(test, target_os = "macos"))]
extern_class!(
    #[unsafe(super(NSObject))]
    #[derive(Debug, PartialEq, Eq, Hash)]
    pub struct SCShareableContent;
);

#[cfg(all(test, target_os = "macos"))]
impl SCShareableContent {
    extern_methods!(
        #[unsafe(method(getShareableContentWithCompletionHandler:))]
        pub fn get(handler: &Block<dyn Fn(*mut SCShareableContent, *mut NSError)>);

        #[unsafe(method(displays))]
        pub fn displays(&self) -> Retained<NSArray<SCDisplay>>;
    );
}
