//! Overlapped control requests; storage remains owned until completion or cancellation drains.

use super::Volume;
use anyhow::{Context, Result, ensure};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::{ffi::c_void, io, mem, ptr};
use usnjmon_core::stop::Stop;
use windows_sys::Win32::Foundation::{
    ERROR_IO_PENDING, ERROR_NOT_FOUND, ERROR_OPERATION_ABORTED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::IO::{
    CancelIoEx, DeviceIoControl, GetOverlappedResult, OVERLAPPED,
};
use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

impl Volume {
    pub(super) fn ioctl(
        &self,
        code: u32,
        input: &[u8],
        size: usize,
        stop: &Stop,
    ) -> Result<Vec<u8>> {
        if stop.requested() {
            return Err(io::Error::from_raw_os_error(ERROR_OPERATION_ABORTED as i32).into());
        }
        // SAFETY: Both pointers are optional NULL values for an unnamed, non-inheritable event.
        let raw_event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if raw_event.is_null() {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: CreateEventW succeeded; transfer unique ownership.
        let event = unsafe { OwnedHandle::from_raw_handle(raw_event) };
        // SAFETY: Zero is the documented initial OVERLAPPED representation.
        let mut overlapped: OVERLAPPED = unsafe { mem::zeroed() };
        overlapped.hEvent = event.as_raw_handle();
        let mut buffer = vec![0u8; size];
        let mut returned = 0;
        // SAFETY: Buffers, event and OVERLAPPED remain in place until completion, including cancellation.
        let success = unsafe {
            DeviceIoControl(
                self.handle.as_raw_handle(),
                code,
                if input.is_empty() {
                    ptr::null()
                } else {
                    input.as_ptr().cast::<c_void>()
                },
                input.len() as u32,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut returned,
                &mut overlapped,
            )
        };
        if success == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                return Err(error).context("journal DeviceIoControl");
            }
            loop {
                // SAFETY: The owned event handle remains open for the full wait.
                let wait = unsafe { WaitForSingleObject(event.as_raw_handle(), 100) };
                if wait == WAIT_OBJECT_0 {
                    break;
                }
                if stop.requested() || wait != WAIT_TIMEOUT {
                    // SAFETY: This is the same live handle/OVERLAPPED pair used for the pending request.
                    let cancel = unsafe { CancelIoEx(self.handle.as_raw_handle(), &overlapped) };
                    if cancel == 0 {
                        let error = io::Error::last_os_error();
                        if error.raw_os_error() != Some(ERROR_NOT_FOUND as i32) {
                            eprintln!(
                                "CancelIoEx failed; draining outstanding I/O before freeing its buffers: {error}"
                            );
                        }
                    }
                    // SAFETY: Keep all request storage live while waiting for cancellation/completion.
                    let completed = unsafe {
                        GetOverlappedResult(
                            self.handle.as_raw_handle(),
                            &overlapped,
                            &mut returned,
                            1,
                        )
                    };
                    if completed == 0 {
                        return Err(io::Error::last_os_error())
                            .context("drain cancelled journal I/O");
                    }
                    ensure!(wait == WAIT_TIMEOUT, "unexpected wait status {wait:#x}");
                    return Err(io::Error::from_raw_os_error(ERROR_OPERATION_ABORTED as i32).into());
                }
            }
        }
        // SAFETY: Request storage remains live and the request has completed or signaled its event.
        if unsafe {
            GetOverlappedResult(self.handle.as_raw_handle(), &overlapped, &mut returned, 0)
        } == 0
        {
            return Err(io::Error::last_os_error()).context("complete journal I/O");
        }
        ensure!(
            returned as usize <= buffer.len(),
            "invalid DeviceIoControl byte count"
        );
        buffer.truncate(returned as usize);
        Ok(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;

    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OVERLAPPED;
    #[test]
    fn native_completion_and_preflight_cancellation_on_owned_fixture_file() {
        let path = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("ioctl-fixture-{}.tmp", std::process::id()));
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .custom_flags(FILE_FLAG_OVERLAPPED)
            .open(&path)
            .unwrap();
        let fixture = Volume {
            handle: file.into(),
            identity: "owned-test-file-not-a-volume".into(),
        };
        const FSCTL_GET_COMPRESSION: u32 = 0x0009003c;
        let result = fixture.ioctl(FSCTL_GET_COMPRESSION, &[], 2, &Stop::default());
        let stop = Stop::default();
        stop.request();
        let cancelled = fixture.ioctl(FSCTL_GET_COMPRESSION, &[], 2, &stop);
        drop(fixture);
        std::fs::remove_file(path).unwrap();
        assert_eq!(result.unwrap().len(), 2);
        assert_eq!(
            cancelled
                .unwrap_err()
                .downcast_ref::<io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(ERROR_OPERATION_ABORTED as i32)
        );
    }
}
