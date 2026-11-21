//! Scheduling hints: run encodes at background QoS (efficiency cores, throttled I/O) and give
//! freed memory back to the system once the queue drains.

use super::ffi::*;

pub fn enter_background_thread() {
    unsafe {
        pthread_set_qos_class_self_np(QOS_CLASS_BACKGROUND, 0);
    }
}

pub fn trim_memory() {
    unsafe {
        malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
    }
}

/// For the short-lived encoder process: put the whole process in the Darwin background band
/// (throttled CPU and I/O, efficiency cores) so conversions never compete with the user.
pub fn enter_background_process() {
    unsafe {
        libc::setpriority(libc::PRIO_DARWIN_PROCESS, 0, libc::PRIO_DARWIN_BG);
    }
    enter_background_thread();
}
