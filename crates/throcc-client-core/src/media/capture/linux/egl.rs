use std::ffi::{CStr, c_char, c_void};
use std::sync::OnceLock;

const PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;
const EXTENSIONS: i32 = 0x3055;
const MODIFIERS_EXTENSION: &str = "EGL_EXT_image_dma_buf_import_modifiers";

type Display = *mut c_void;
type GetProcAddress = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type GetPlatformDisplay = unsafe extern "C" fn(u32, *mut c_void, *const i32) -> Display;
type Initialize = unsafe extern "C" fn(Display, *mut i32, *mut i32) -> u32;
type Terminate = unsafe extern "C" fn(Display) -> u32;
type QueryString = unsafe extern "C" fn(Display, i32) -> *const c_char;
type QueryModifiers = unsafe extern "C" fn(Display, i32, i32, *mut u64, *mut u32, *mut i32) -> u32;

/// The DRM format modifiers the GPU can import pixels of `fourcc` with, as EGL reports
/// them. Empty without EGL or its modifier extension.
pub fn dmabuf_modifiers(fourcc: u32) -> Vec<u64> {
    static ALL: OnceLock<Vec<(u32, Vec<u64>)>> = OnceLock::new();
    ALL.get_or_init(|| {
        // SAFETY: `query` only calls EGL entry points with the signatures EGL defines.
        unsafe { query(&super::DMABUF_FOURCCS) }.unwrap_or_default()
    })
    .iter()
    .find(|(known, _)| *known == fourcc)
    .map(|(_, modifiers)| modifiers.clone())
    .unwrap_or_default()
}

/// # Safety
/// libEGL.so.1, when present, must be a conforming EGL implementation.
unsafe fn query(fourccs: &[u32]) -> Option<Vec<(u32, Vec<u64>)>> {
    // SAFETY: the library stays loaded for the process, so the symbols remain valid.
    let library =
        unsafe { libc::dlopen(c"libEGL.so.1".as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
    if library.is_null() {
        return None;
    }
    let symbol = |name: &CStr| {
        // SAFETY: `library` is a live handle and `name` is NUL-terminated.
        let address = unsafe { libc::dlsym(library, name.as_ptr()) };
        (!address.is_null()).then_some(address)
    };
    let get_proc: GetProcAddress = unsafe { std::mem::transmute(symbol(c"eglGetProcAddress")?) };
    let extension = |name: &CStr| {
        // SAFETY: eglGetProcAddress takes a NUL-terminated name.
        let address = unsafe { get_proc(name.as_ptr()) };
        (!address.is_null()).then_some(address)
    };
    // SAFETY: each address is the EGL function of that name, whose C signature the type
    // spells out.
    let (get_display, initialize, terminate, query_string, query_modifiers) = unsafe {
        (
            std::mem::transmute::<*mut c_void, GetPlatformDisplay>(extension(
                c"eglGetPlatformDisplayEXT",
            )?),
            std::mem::transmute::<*mut c_void, Initialize>(symbol(c"eglInitialize")?),
            std::mem::transmute::<*mut c_void, Terminate>(symbol(c"eglTerminate")?),
            std::mem::transmute::<*mut c_void, QueryString>(symbol(c"eglQueryString")?),
            std::mem::transmute::<*mut c_void, QueryModifiers>(extension(
                c"eglQueryDmaBufModifiersEXT",
            )?),
        )
    };
    // SAFETY: the calls below follow the EGL specification: a display is initialised
    // before use and terminated after, and every out pointer has room for what it gets.
    unsafe {
        let display = get_display(
            PLATFORM_SURFACELESS_MESA,
            std::ptr::null_mut(),
            std::ptr::null(),
        );
        let (mut major, mut minor) = (0, 0);
        if display.is_null() || initialize(display, &mut major, &mut minor) == 0 {
            return None;
        }
        let extensions = query_string(display, EXTENSIONS);
        let supported = !extensions.is_null()
            && CStr::from_ptr(extensions)
                .to_str()
                .is_ok_and(|names| names.split(' ').any(|name| name == MODIFIERS_EXTENSION));
        let found = supported.then(|| {
            fourccs
                .iter()
                .map(|&fourcc| (fourcc, modifiers_of(display, query_modifiers, fourcc)))
                .collect()
        });
        terminate(display);
        found
    }
}

/// # Safety
/// `display` must be an initialised EGL display and `query` its eglQueryDmaBufModifiersEXT.
unsafe fn modifiers_of(display: Display, query: QueryModifiers, fourcc: u32) -> Vec<u64> {
    let null = std::ptr::null_mut();
    let mut count = 0;
    // SAFETY: with a capacity of zero EGL only writes the count.
    if unsafe { query(display, fourcc as i32, 0, null, null.cast(), &mut count) } == 0 {
        return Vec::new();
    }
    let mut modifiers = vec![0u64; usize::try_from(count).unwrap_or(0)];
    // SAFETY: `modifiers` has room for `count` entries, and the external-only flags are not
    // asked for.
    let filled = unsafe {
        query(
            display,
            fourcc as i32,
            count,
            modifiers.as_mut_ptr(),
            null.cast(),
            &mut count,
        )
    };
    if filled == 0 {
        return Vec::new();
    }
    modifiers.truncate(usize::try_from(count).unwrap_or(0));
    modifiers
}
