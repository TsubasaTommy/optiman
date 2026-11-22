//! Background priority (low CPU + I/O priority) and EcoQoS for encoder threads, EcoQoS and very
//! low memory priority for the resident agent, and working-set trimming once the queue drains.

use std::ffi::c_void;

use windows::Win32::System::Memory::{
    GetProcessHeap, HEAP_FLAGS, HeapCompact, SETPROCESSWORKINGSETSIZEEX_FLAGS, SetProcessWorkingSetSizeEx,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, MEMORY_PRIORITY_INFORMATION, MEMORY_PRIORITY_VERY_LOW,
    PROCESS_MODE_BACKGROUND_BEGIN, PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_STATE, ProcessMemoryPriority, ProcessPowerThrottling, SetPriorityClass,
    SetProcessInformation, SetThreadInformation, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
    THREAD_POWER_THROTTLING_CURRENT_VERSION, THREAD_POWER_THROTTLING_EXECUTION_SPEED, THREAD_POWER_THROTTLING_STATE,
    ThreadPowerThrottling,
};

pub fn enter_background_thread() {
    unsafe {
        let thread = GetCurrentThread();
        let _ = SetThreadPriority(thread, THREAD_MODE_BACKGROUND_BEGIN);
        // EcoQoS (Windows 11): schedule on efficiency cores at low clock. Ignored on older systems.
        let state = THREAD_POWER_THROTTLING_STATE {
            Version: THREAD_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED,
        };
        let _ = SetThreadInformation(
            thread,
            ThreadPowerThrottling,
            (&raw const state).cast::<c_void>(),
            size_of::<THREAD_POWER_THROTTLING_STATE>() as u32,
        );
    }
}

pub fn trim_memory() {
    unsafe {
        if let Ok(heap) = GetProcessHeap() {
            HeapCompact(heap, HEAP_FLAGS(0));
        }
        // (SIZE_T)-1 for both limits: remove as many pages as possible from the working set.
        let _ = SetProcessWorkingSetSizeEx(
            GetCurrentProcess(),
            usize::MAX,
            usize::MAX,
            SETPROCESSWORKINGSETSIZEEX_FLAGS(0),
        );
    }
}

fn process_eco_qos() {
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    };
    unsafe {
        let _ = SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            (&raw const state).cast::<c_void>(),
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        );
    }
}

/// For the resident agent, which mostly waits: EcoQoS and the lowest memory priority, so its few
/// pages are the first to be reclaimed. Its CPU priority stays normal for prompt reactions.
pub fn enter_resident_mode() {
    process_eco_qos();
    let info = MEMORY_PRIORITY_INFORMATION { MemoryPriority: MEMORY_PRIORITY_VERY_LOW };
    unsafe {
        let _ = SetProcessInformation(
            GetCurrentProcess(),
            ProcessMemoryPriority,
            (&raw const info).cast::<c_void>(),
            size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
        );
    }
}

/// For the short-lived encoder process: background mode (very low CPU, I/O and memory
/// priority) plus process-wide EcoQoS.
pub fn enter_background_process() {
    unsafe {
        let _ = SetPriorityClass(GetCurrentProcess(), PROCESS_MODE_BACKGROUND_BEGIN);
    }
    process_eco_qos();
    enter_background_thread();
}
