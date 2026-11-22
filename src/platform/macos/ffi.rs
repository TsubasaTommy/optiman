//! Hand-written bindings for the small slices of CoreFoundation, ImageIO, CoreGraphics,
//! CoreServices (FSEvents), libdispatch and the Objective-C runtime that we use.

#![allow(non_upper_case_globals, non_snake_case, non_camel_case_types, dead_code)]

use std::ffi::{CStr, c_char, c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

pub type CFTypeRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFMutableDictionaryRef = *mut c_void;
pub type CFDataRef = *const c_void;
pub type CFMutableDataRef = *mut c_void;
pub type CFArrayRef = *const c_void;
pub type CFNumberRef = *const c_void;
pub type CFURLRef = *const c_void;
pub type CFBooleanRef = *const c_void;
pub type CFIndex = isize;
pub type CFTypeID = usize;
pub type Boolean = u8;

pub type CGImageSourceRef = *mut c_void;
pub type CGImageDestinationRef = *mut c_void;
pub type CGImageRef = *mut c_void;
pub type CGColorSpaceRef = *mut c_void;
pub type CGContextRef = *mut c_void;

#[repr(C)]
pub struct CFDictionaryKeyCallBacks {
    version: CFIndex,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
    equal: *const c_void,
    hash: *const c_void,
}

#[repr(C)]
pub struct CFDictionaryValueCallBacks {
    version: CFIndex,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
    equal: *const c_void,
}

#[repr(C)]
pub struct CFArrayCallBacks {
    version: CFIndex,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
    equal: *const c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

pub const kCFStringEncodingUTF8: u32 = 0x0800_0100;
pub const kCFNumberSInt64Type: CFIndex = 4;
pub const kCFNumberDoubleType: CFIndex = 13;

// CGImageAlphaInfo / CGBitmapInfo
pub const kCGImageAlphaNone: u32 = 0;
pub const kCGImageAlphaPremultipliedLast: u32 = 1;
pub const kCGImageAlphaNoneSkipLast: u32 = 5;
pub const kCGImageAlphaNoneSkipFirst: u32 = 6;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub static kCFAllocatorDefault: CFAllocatorRef;
    pub static kCFTypeDictionaryKeyCallBacks: CFDictionaryKeyCallBacks;
    pub static kCFTypeDictionaryValueCallBacks: CFDictionaryValueCallBacks;
    pub static kCFTypeArrayCallBacks: CFArrayCallBacks;
    pub static kCFBooleanTrue: CFBooleanRef;
    pub static kCFBooleanFalse: CFBooleanRef;

    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFEqual(a: CFTypeRef, b: CFTypeRef) -> Boolean;
    pub fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
    pub fn CFStringGetTypeID() -> CFTypeID;
    pub fn CFDictionaryGetTypeID() -> CFTypeID;
    pub fn CFNumberGetTypeID() -> CFTypeID;

    pub fn CFStringCreateWithBytes(
        alloc: CFAllocatorRef,
        bytes: *const u8,
        num_bytes: CFIndex,
        encoding: u32,
        is_external: Boolean,
    ) -> CFStringRef;
    pub fn CFStringGetLength(s: CFStringRef) -> CFIndex;
    pub fn CFStringGetMaximumSizeForEncoding(length: CFIndex, encoding: u32) -> CFIndex;
    pub fn CFStringGetCString(s: CFStringRef, buffer: *mut c_char, size: CFIndex, encoding: u32) -> Boolean;

    pub fn CFDataCreate(alloc: CFAllocatorRef, bytes: *const u8, length: CFIndex) -> CFDataRef;
    pub fn CFDataCreateMutable(alloc: CFAllocatorRef, capacity: CFIndex) -> CFMutableDataRef;
    pub fn CFDataGetBytePtr(data: CFDataRef) -> *const u8;
    pub fn CFDataGetLength(data: CFDataRef) -> CFIndex;

    pub fn CFDictionaryCreateMutable(
        alloc: CFAllocatorRef,
        capacity: CFIndex,
        key_callbacks: *const CFDictionaryKeyCallBacks,
        value_callbacks: *const CFDictionaryValueCallBacks,
    ) -> CFMutableDictionaryRef;
    pub fn CFDictionaryCreateMutableCopy(
        alloc: CFAllocatorRef,
        capacity: CFIndex,
        dict: CFDictionaryRef,
    ) -> CFMutableDictionaryRef;
    pub fn CFDictionaryGetValue(dict: CFDictionaryRef, key: *const c_void) -> *const c_void;
    pub fn CFDictionarySetValue(dict: CFMutableDictionaryRef, key: *const c_void, value: *const c_void);
    pub fn CFDictionaryRemoveValue(dict: CFMutableDictionaryRef, key: *const c_void);

    pub fn CFNumberCreate(alloc: CFAllocatorRef, the_type: CFIndex, value_ptr: *const c_void) -> CFNumberRef;
    pub fn CFNumberGetValue(number: CFNumberRef, the_type: CFIndex, value_ptr: *mut c_void) -> Boolean;

    pub fn CFArrayCreate(
        alloc: CFAllocatorRef,
        values: *const *const c_void,
        num_values: CFIndex,
        callbacks: *const CFArrayCallBacks,
    ) -> CFArrayRef;
    pub fn CFArrayGetCount(array: CFArrayRef) -> CFIndex;
    pub fn CFArrayGetValueAtIndex(array: CFArrayRef, idx: CFIndex) -> *const c_void;

    pub fn CFURLCreateFromFileSystemRepresentation(
        alloc: CFAllocatorRef,
        buffer: *const u8,
        buf_len: CFIndex,
        is_directory: Boolean,
    ) -> CFURLRef;
}

#[link(name = "ImageIO", kind = "framework")]
unsafe extern "C" {
    pub static kCGImagePropertyOrientation: CFStringRef;
    pub static kCGImagePropertyPixelWidth: CFStringRef;
    pub static kCGImagePropertyPixelHeight: CFStringRef;
    pub static kCGImagePropertyTIFFDictionary: CFStringRef;
    pub static kCGImagePropertyTIFFOrientation: CFStringRef;
    pub static kCGImageSourceShouldCache: CFStringRef;
    pub static kCGImageSourceShouldCacheImmediately: CFStringRef;
    pub static kCGImageSourceCreateThumbnailFromImageAlways: CFStringRef;
    pub static kCGImageSourceCreateThumbnailWithTransform: CFStringRef;
    pub static kCGImageSourceThumbnailMaxPixelSize: CFStringRef;
    pub static kCGImageDestinationLossyCompressionQuality: CFStringRef;

    pub fn CGImageSourceCreateWithData(data: CFDataRef, options: CFDictionaryRef) -> CGImageSourceRef;
    pub fn CGImageSourceGetStatus(src: CGImageSourceRef) -> i32;
    pub fn CGImageSourceGetCount(src: CGImageSourceRef) -> usize;
    pub fn CGImageSourceCopyPropertiesAtIndex(
        src: CGImageSourceRef,
        index: usize,
        options: CFDictionaryRef,
    ) -> CFDictionaryRef;
    pub fn CGImageSourceCreateImageAtIndex(src: CGImageSourceRef, index: usize, options: CFDictionaryRef)
    -> CGImageRef;
    pub fn CGImageSourceCreateThumbnailAtIndex(
        src: CGImageSourceRef,
        index: usize,
        options: CFDictionaryRef,
    ) -> CGImageRef;
    pub fn CGImageSourceCopyTypeIdentifiers() -> CFArrayRef;

    pub fn CGImageDestinationCopyTypeIdentifiers() -> CFArrayRef;
    pub fn CGImageDestinationCreateWithData(
        data: CFMutableDataRef,
        type_: CFStringRef,
        count: usize,
        options: CFDictionaryRef,
    ) -> CGImageDestinationRef;
    pub fn CGImageDestinationAddImage(dest: CGImageDestinationRef, image: CGImageRef, properties: CFDictionaryRef);
    pub fn CGImageDestinationFinalize(dest: CGImageDestinationRef) -> bool;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    pub static kCGColorSpaceSRGB: CFStringRef;
    pub static kCGColorSpaceDisplayP3: CFStringRef;

    pub fn CGImageGetWidth(image: CGImageRef) -> usize;
    pub fn CGImageGetHeight(image: CGImageRef) -> usize;
    pub fn CGImageGetColorSpace(image: CGImageRef) -> CGColorSpaceRef;
    pub fn CGImageGetAlphaInfo(image: CGImageRef) -> u32;
    pub fn CGImageGetBitmapInfo(image: CGImageRef) -> u32;
    pub fn CGImageGetBitsPerComponent(image: CGImageRef) -> usize;
    pub fn CGImageGetBitsPerPixel(image: CGImageRef) -> usize;
    pub fn CGImageGetBytesPerRow(image: CGImageRef) -> usize;
    pub fn CGImageGetDataProvider(image: CGImageRef) -> *mut c_void;
    pub fn CGDataProviderCopyData(provider: *mut c_void) -> CFDataRef;
    pub fn CGColorSpaceCreateWithName(name: CFStringRef) -> CGColorSpaceRef;
    pub fn CGColorSpaceIsWideGamutRGB(space: CGColorSpaceRef) -> bool;
    pub fn CGColorSpaceGetModel(space: CGColorSpaceRef) -> i32;
    pub fn CGColorSpaceGetBaseColorSpace(space: CGColorSpaceRef) -> CGColorSpaceRef;
    pub fn CGColorSpaceCopyName(space: CGColorSpaceRef) -> CFStringRef;
    pub fn CGColorSpaceCopyICCData(space: CGColorSpaceRef) -> CFDataRef;
    pub fn CGColorSpaceCreateWithICCData(data: CFTypeRef) -> CGColorSpaceRef;
    pub fn CGDataProviderCreateWithCFData(data: CFDataRef) -> *mut c_void;
    pub fn CGImageCreate(
        width: usize,
        height: usize,
        bits_per_component: usize,
        bits_per_pixel: usize,
        bytes_per_row: usize,
        space: CGColorSpaceRef,
        bitmap_info: u32,
        provider: *mut c_void,
        decode: *const f64,
        should_interpolate: bool,
        intent: i32,
    ) -> CGImageRef;
    pub fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: CGColorSpaceRef,
        bitmap_info: u32,
    ) -> CGContextRef;
    pub fn CGContextDrawImage(ctx: CGContextRef, rect: CGRect, image: CGImageRef);
}

// ---- CoreServices: FSEvents ----

pub type FSEventStreamRef = *mut c_void;
pub type FSEventStreamEventId = u64;
pub type FSEventStreamCallback = extern "C" fn(
    stream: FSEventStreamRef,
    info: *mut c_void,
    num_events: usize,
    event_paths: *mut c_void,
    event_flags: *const u32,
    event_ids: *const FSEventStreamEventId,
);

#[repr(C)]
pub struct FSEventStreamContext {
    pub version: CFIndex,
    pub info: *mut c_void,
    pub retain: *const c_void,
    pub release: *const c_void,
    pub copy_description: *const c_void,
}

pub const kFSEventStreamEventIdSinceNow: FSEventStreamEventId = u64::MAX;

pub const kFSEventStreamCreateFlagWatchRoot: u32 = 0x0000_0004;
pub const kFSEventStreamCreateFlagIgnoreSelf: u32 = 0x0000_0008;
pub const kFSEventStreamCreateFlagFileEvents: u32 = 0x0000_0010;

pub const kFSEventStreamEventFlagMustScanSubDirs: u32 = 0x0000_0001;
pub const kFSEventStreamEventFlagUserDropped: u32 = 0x0000_0002;
pub const kFSEventStreamEventFlagKernelDropped: u32 = 0x0000_0004;
pub const kFSEventStreamEventFlagHistoryDone: u32 = 0x0000_0010;
pub const kFSEventStreamEventFlagRootChanged: u32 = 0x0000_0020;
pub const kFSEventStreamEventFlagItemCreated: u32 = 0x0000_0100;
pub const kFSEventStreamEventFlagItemRenamed: u32 = 0x0000_0800;
pub const kFSEventStreamEventFlagItemModified: u32 = 0x0000_1000;
pub const kFSEventStreamEventFlagItemIsFile: u32 = 0x0001_0000;
pub const kFSEventStreamEventFlagItemIsDir: u32 = 0x0002_0000;
pub const kFSEventStreamEventFlagItemCloned: u32 = 0x0040_0000;

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    pub fn FSEventStreamCreate(
        allocator: CFAllocatorRef,
        callback: FSEventStreamCallback,
        context: *mut FSEventStreamContext,
        paths_to_watch: CFArrayRef,
        since_when: FSEventStreamEventId,
        latency: f64,
        flags: u32,
    ) -> FSEventStreamRef;
    pub fn FSEventStreamSetDispatchQueue(stream: FSEventStreamRef, queue: DispatchQueue);
    pub fn FSEventStreamSetExclusionPaths(stream: FSEventStreamRef, paths: CFArrayRef) -> Boolean;
    pub fn FSEventStreamStart(stream: FSEventStreamRef) -> Boolean;
    pub fn FSEventStreamStop(stream: FSEventStreamRef);
    pub fn FSEventStreamInvalidate(stream: FSEventStreamRef);
    pub fn FSEventStreamRelease(stream: FSEventStreamRef);
    pub fn FSEventsGetCurrentEventId() -> FSEventStreamEventId;
}

// ---- libdispatch (part of libSystem) ----

pub type DispatchQueue = *mut c_void;

unsafe extern "C" {
    pub fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> DispatchQueue;
    pub fn dispatch_sync_f(queue: DispatchQueue, context: *mut c_void, work: extern "C" fn(*mut c_void));
    pub fn dispatch_release(object: *mut c_void);
}

// ---- libSystem misc ----

pub const QOS_CLASS_BACKGROUND: u32 = 0x09;

unsafe extern "C" {
    pub fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: c_int) -> c_int;
    pub fn malloc_zone_pressure_relief(zone: *mut c_void, goal: usize) -> usize;
}

// ---- Objective-C runtime (for NSFileManager trash) ----

#[link(name = "objc")]
unsafe extern "C" {
    pub fn objc_getClass(name: *const c_char) -> *mut c_void;
    pub fn sel_registerName(name: *const c_char) -> *mut c_void;
    pub fn objc_msgSend();
    pub fn objc_autoreleasePoolPush() -> *mut c_void;
    pub fn objc_autoreleasePoolPop(pool: *mut c_void);
}

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}

// ---- Safe-ish helpers ----

/// An owned (+1) CoreFoundation reference, released on drop.
pub struct Cf(CFTypeRef);

impl Cf {
    /// Takes ownership of a reference obtained from a Create/Copy function.
    ///
    /// # Safety
    /// `p` must be null or a +1 CoreFoundation reference.
    pub unsafe fn from_create(p: *const c_void) -> Option<Cf> {
        if p.is_null() { None } else { Some(Cf(p)) }
    }

    pub fn ptr(&self) -> CFTypeRef {
        self.0
    }

    pub fn mut_ptr(&self) -> *mut c_void {
        self.0 as *mut c_void
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}

pub fn cf_string(s: &str) -> Cf {
    unsafe {
        let p = CFStringCreateWithBytes(kCFAllocatorDefault, s.as_ptr(), s.len() as CFIndex, kCFStringEncodingUTF8, 0);
        Cf::from_create(p).expect("CFStringCreateWithBytes failed")
    }
}

pub fn cf_data(bytes: &[u8]) -> Cf {
    unsafe {
        Cf::from_create(CFDataCreate(kCFAllocatorDefault, bytes.as_ptr(), bytes.len() as CFIndex))
            .expect("CFDataCreate failed")
    }
}

/// # Safety
/// `data` must be a valid CFData reference that outlives the returned slice.
pub unsafe fn cf_data_bytes<'a>(data: CFDataRef) -> &'a [u8] {
    unsafe {
        let len = CFDataGetLength(data) as usize;
        if len == 0 {
            return &[];
        }
        std::slice::from_raw_parts(CFDataGetBytePtr(data), len)
    }
}

pub fn cf_i64(v: i64) -> Cf {
    unsafe {
        Cf::from_create(CFNumberCreate(kCFAllocatorDefault, kCFNumberSInt64Type, (&raw const v).cast::<c_void>()))
            .expect("CFNumberCreate failed")
    }
}

pub fn cf_f64(v: f64) -> Cf {
    unsafe {
        Cf::from_create(CFNumberCreate(kCFAllocatorDefault, kCFNumberDoubleType, (&raw const v).cast::<c_void>()))
            .expect("CFNumberCreate failed")
    }
}

pub fn cf_url(path: &Path) -> Option<Cf> {
    let bytes = path.as_os_str().as_bytes();
    unsafe {
        Cf::from_create(CFURLCreateFromFileSystemRepresentation(
            kCFAllocatorDefault,
            bytes.as_ptr(),
            bytes.len() as CFIndex,
            Boolean::from(path.is_dir()),
        ))
    }
}

pub fn cf_mutable_dict() -> Cf {
    unsafe {
        Cf::from_create(CFDictionaryCreateMutable(
            kCFAllocatorDefault,
            0,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        ))
        .expect("CFDictionaryCreateMutable failed")
    }
}

/// Builds an immutable-use dictionary from (key, value) pairs.
pub fn cf_dict(pairs: &[(CFStringRef, CFTypeRef)]) -> Cf {
    let d = cf_mutable_dict();
    for (k, v) in pairs {
        unsafe { CFDictionarySetValue(d.mut_ptr(), *k, *v) };
    }
    d
}

/// # Safety
/// `s` must be null or a valid CFString.
pub unsafe fn cf_string_to_rust(s: CFStringRef) -> Option<String> {
    unsafe {
        if s.is_null() || CFGetTypeID(s) != CFStringGetTypeID() {
            return None;
        }
        let max = CFStringGetMaximumSizeForEncoding(CFStringGetLength(s), kCFStringEncodingUTF8) + 1;
        let mut buf = vec![0 as c_char; max as usize];
        if CFStringGetCString(s, buf.as_mut_ptr(), max, kCFStringEncodingUTF8) == 0 {
            return None;
        }
        Some(CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned())
    }
}

/// # Safety
/// `dict` must be a valid CFDictionary.
pub unsafe fn cf_dict_i64(dict: CFDictionaryRef, key: CFStringRef) -> Option<i64> {
    unsafe {
        let v = CFDictionaryGetValue(dict, key);
        if v.is_null() || CFGetTypeID(v) != CFNumberGetTypeID() {
            return None;
        }
        let mut out: i64 = 0;
        (CFNumberGetValue(v, kCFNumberSInt64Type, (&raw mut out).cast::<c_void>()) != 0).then_some(out)
    }
}

/// # Safety
/// `array` must be a valid CFArray of CFStrings.
pub unsafe fn cf_array_contains_str(array: CFArrayRef, needle: &str) -> bool {
    unsafe {
        (0..CFArrayGetCount(array))
            .any(|i| cf_string_to_rust(CFArrayGetValueAtIndex(array, i)).as_deref() == Some(needle))
    }
}
