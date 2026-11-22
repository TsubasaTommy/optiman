//! Consumes `com.apple.fsevents.matching` launch events from launchd.
//!
//! launchd launches the agent when a watched folder changes; the job must accept the event with
//! `xpc_set_event_stream_handler`, or launchd keeps relaunching it. The handler is an Objective-C
//! block; a global (capture-free) block literal is enough, so no block runtime crate is needed.

use std::ffi::{c_char, c_int, c_ulong, c_void};
use std::sync::OnceLock;

use super::events::Notifier;
use super::ffi::{DispatchQueue, dispatch_queue_create};
use crate::event::Msg;

const STREAM: &std::ffi::CStr = c"com.apple.fsevents.matching";
const BLOCK_IS_GLOBAL: c_int = 1 << 28;

#[repr(C)]
struct BlockDescriptor {
    reserved: c_ulong,
    size: c_ulong,
}

#[repr(C)]
struct BlockLiteral {
    isa: *const c_void,
    flags: c_int,
    reserved: c_int,
    invoke: unsafe extern "C" fn(*const BlockLiteral, *mut c_void),
    descriptor: *const BlockDescriptor,
}

/// The block is immutable global data, like the ones clang emits for capture-free blocks.
struct GlobalBlock(BlockLiteral);
unsafe impl Sync for GlobalBlock {}

unsafe extern "C" {
    static _NSConcreteGlobalBlock: [*const c_void; 32];
    fn xpc_set_event_stream_handler(stream: *const c_char, targetq: DispatchQueue, handler: *const c_void);
}

static DESCRIPTOR: BlockDescriptor = BlockDescriptor { reserved: 0, size: size_of::<BlockLiteral>() as c_ulong };

static HANDLER: GlobalBlock = GlobalBlock(BlockLiteral {
    isa: (&raw const _NSConcreteGlobalBlock).cast(),
    flags: BLOCK_IS_GLOBAL,
    reserved: 0,
    invoke: on_event,
    descriptor: &raw const DESCRIPTOR,
});

static NOTIFIER: OnceLock<Notifier> = OnceLock::new();

unsafe extern "C" fn on_event(_block: *const BlockLiteral, _event: *mut c_void) {
    if let Some(n) = NOTIFIER.get() {
        n.send(Msg::Wake);
    }
}

pub fn consume_fsevents_launch_events(notifier: Notifier) {
    if NOTIFIER.set(notifier).is_err() {
        return; // already registered
    }
    unsafe {
        let queue =
            dispatch_queue_create(c"io.github.tsubasatommy.optimize-image-folder.launch".as_ptr(), std::ptr::null());
        xpc_set_event_stream_handler(STREAM.as_ptr(), queue, (&raw const HANDLER.0).cast());
    }
}
