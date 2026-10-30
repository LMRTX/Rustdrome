//! Tie mpv's lifetime to ours on Windows.
//!
//! mpv is started with `CREATE_NO_WINDOW`, so it owns a hidden console of its own and
//! closing our terminal does nothing to it: when we are killed it just keeps idling.
//! A job object with `KILL_ON_JOB_CLOSE` fixes that for every way of dying. Windows
//! closes our handle to the job when the process ends (even when it is terminated), and
//! that kills everything in the job.

use std::mem::size_of;
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

/// The job's handle, kept open on purpose for the life of the process. Never closed by us:
/// the OS does it at exit, which is the moment we want mpv to go.
static JOB: OnceLock<isize> = OnceLock::new();

/// Put the process behind `process` (a `HANDLE`) into a job that dies with us.
/// Best effort: if any step fails mpv simply isn't covered and the normal `Drop` path
/// still applies.
pub fn kill_with_us(process: isize) {
    if JOB.get().is_some() {
        return;
    }
    // SAFETY: plain Win32 calls on handles we own; the struct is zero-initialised, which is
    // a valid value for it, and its size is passed exactly.
    unsafe {
        let job: HANDLE = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job == 0 {
            return;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let set = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if set == 0 || AssignProcessToJobObject(job, process as HANDLE) == 0 {
            CloseHandle(job);
            return;
        }
        let _ = JOB.set(job);
    }
}
