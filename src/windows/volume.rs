use crate::{engine::Journal, journal::BUFFER_SIZE, stop::Stop};
use anyhow::{Context, Result, ensure};
use std::{
    ffi::c_void,
    io,
    mem::zeroed,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{
        ERROR_IO_PENDING, ERROR_NOT_FOUND, ERROR_OPERATION_ABORTED, GENERIC_READ,
        INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        GetVolumeInformationByHandleW, GetVolumeNameForVolumeMountPointW, OPEN_EXISTING,
    },
    System::{
        IO::{CancelIoEx, DeviceIoControl, GetOverlappedResult, OVERLAPPED},
        Threading::{CreateEventW, WaitForSingleObject},
    },
};

const FSCTL_QUERY_USN_JOURNAL: u32 = 0x000900f4;
const FSCTL_READ_USN_JOURNAL: u32 = 0x000900bb;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub struct Volume {
    handle: OwnedHandle,
    pub identity: String,
}

pub struct Query {
    pub bounds: Journal,
    maximum_version: u16,
}

impl Volume {
    pub fn open(root: &str) -> Result<Self> {
        let mut guid = [0u16; 128];
        if unsafe {
            GetVolumeNameForVolumeMountPointW(
                wide(root).as_ptr(),
                guid.as_mut_ptr(),
                guid.len() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error())
                .context("resolve configured mount point to volume GUID");
        }
        let guid = String::from_utf16(&guid[..guid.iter().position(|&c| c == 0).unwrap()])?
            .to_ascii_lowercase();
        let path = wide(guid.trim_end_matches('\\'));
        let raw = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error())
                .context("open volume read-only (elevation required)");
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut serial = 0;
        let mut filesystem = [0u16; 32];
        if unsafe {
            GetVolumeInformationByHandleW(
                handle.as_raw_handle(),
                null_mut(),
                0,
                &mut serial,
                null_mut(),
                null_mut(),
                filesystem.as_mut_ptr(),
                filesystem.len() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error())
                .context("query opened volume identity and filesystem");
        }
        let fs =
            String::from_utf16(&filesystem[..filesystem.iter().position(|&c| c == 0).unwrap()])?;
        ensure!(
            fs == "NTFS",
            "unsupported filesystem {fs}; only local NTFS is supported"
        );
        Ok(Self {
            handle,
            identity: format!("{}:{serial:08x}", guid.trim_end_matches('\\')),
        })
    }

    pub fn query(&self, stop: &Stop) -> Result<Query> {
        let bytes = self.ioctl(FSCTL_QUERY_USN_JOURNAL, &[], 80, stop)?;
        ensure!(bytes.len() >= 56, "truncated USN_JOURNAL_DATA");
        let i64_at = |n| i64::from_le_bytes(bytes[n..n + 8].try_into().unwrap());
        let maximum_version = if bytes.len() >= 60 {
            u16::from_le_bytes(bytes[58..60].try_into().unwrap())
        } else {
            2
        };
        ensure!(
            maximum_version >= 2,
            "journal reports unsupported maximum record version {maximum_version}"
        );
        let bounds = Journal {
            id: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            first_usn: i64_at(8),
            next_usn: i64_at(16),
            lowest_valid_usn: i64_at(24),
        };
        bounds.oldest()?;
        Ok(Query {
            bounds,
            maximum_version,
        })
    }

    pub fn read(&self, cursor: i64, journal: &Query, stop: &Stop) -> Result<Vec<u8>> {
        // READ_USN_JOURNAL_DATA_V1: no close-only filter, no waiting for future bytes.
        let mut request = [0u8; 48];
        request[..8].copy_from_slice(&cursor.to_le_bytes());
        request[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        request[32..40].copy_from_slice(&journal.bounds.id.to_le_bytes());
        request[40..42].copy_from_slice(&2u16.to_le_bytes());
        request[42..44].copy_from_slice(&journal.maximum_version.to_le_bytes());
        self.ioctl(FSCTL_READ_USN_JOURNAL, &request, BUFFER_SIZE, stop)
    }

    fn ioctl(&self, code: u32, input: &[u8], size: usize, stop: &Stop) -> Result<Vec<u8>> {
        if stop.requested() {
            return Err(io::Error::from_raw_os_error(ERROR_OPERATION_ABORTED as i32).into());
        }
        let raw_event = unsafe { CreateEventW(null(), 1, 0, null()) };
        if raw_event.is_null() {
            return Err(io::Error::last_os_error().into());
        }
        let event = unsafe { OwnedHandle::from_raw_handle(raw_event) };
        let mut overlapped: OVERLAPPED = unsafe { zeroed() };
        overlapped.hEvent = event.as_raw_handle();
        let mut buffer = vec![0u8; size];
        let mut returned = 0;
        let success = unsafe {
            DeviceIoControl(
                self.handle.as_raw_handle(),
                code,
                if input.is_empty() {
                    null()
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
                let wait = unsafe { WaitForSingleObject(event.as_raw_handle(), 100) };
                if wait == WAIT_OBJECT_0 {
                    break;
                }
                if stop.requested() || wait != WAIT_TIMEOUT {
                    let cancel = unsafe { CancelIoEx(self.handle.as_raw_handle(), &overlapped) };
                    if cancel == 0 {
                        let error = io::Error::last_os_error();
                        if error.raw_os_error() != Some(ERROR_NOT_FOUND as i32) {
                            eprintln!(
                                "CancelIoEx failed; draining outstanding I/O before freeing its buffers: {error}"
                            );
                        }
                    }
                    // Even cancellation must complete before the kernel-owned buffers can be dropped.
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
