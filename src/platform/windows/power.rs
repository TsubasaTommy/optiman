//! Background priority (low CPU + I/O priority) and EcoQoS for encoder threads, and working-set
//! trimming once the queue drains.

use std::ffi::c_void;

use windows::Win32::System::Memory::{
    GetProcessHeap, HEAP_FLAGS, HeapCompact, SETPROCESSWORKINGSETSIZEEX_FLAGS, SetProcessWorkingSetSizeEx,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, PROCESS_MODE_BACKGROUND_BEGIN, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetPriorityClass,
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
            &state as *const THREAD_POWER_THROTTLING_STATE as *const c_void,
            std::mem::size_of::<THREAD_POWER_THROTTLING_STATE>() as u32,
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

/// For the short-lived encoder process: background mode (very low CPU, I/O and memory
/// priority) plus process-wide EcoQoS.
pub fn enter_background_process() {
    unsafe {
        let process = GetCurrentProcess();
        let _ = SetPriorityClass(process, PROCESS_MODE_BACKGROUND_BEGIN);
        let state = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        };
        let _ = SetProcessInformation(
            process,
            ProcessPowerThrottling,
            &state as *const PROCESS_POWER_THROTTLING_STATE as *const c_void,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        );
    }
    enter_background_thread();
}
