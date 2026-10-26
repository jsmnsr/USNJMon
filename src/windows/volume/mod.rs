//! Read-only volume ownership, identity validation and journal control messages.

mod io;
mod path;

use super::wide;
use anyhow::{Context, Result, ensure};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::{io as std_io, ptr};
use usnjmon_core::engine::Journal;
use usnjmon_core::journal::BUFFER_SIZE;
use usnjmon_core::stop::Stop;
use windows_sys::Win32::Foundation::{GENERIC_READ, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GetVolumeInformationByHandleW, OPEN_EXISTING,
};

const FSCTL_QUERY_USN_JOURNAL: u32 = 0x000900f4;
const FSCTL_READ_USN_JOURNAL: u32 = 0x000900bb;

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
        let guid = super::discovery::resolve_root(root)?;
        let path = wide(guid.trim_end_matches('\\'));
        // SAFETY: Path is NUL-terminated; other pointers are optional NULL values.
        let raw = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(std_io::Error::last_os_error())
                .context("open volume read-only (elevation required)");
        }
        // SAFETY: CreateFileW succeeded and ownership is transferred exactly once.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut serial = 0;
        let mut filesystem = [0u16; 32];
        // SAFETY: The owned handle and all writable outputs remain valid with accurate lengths.
        if unsafe {
            GetVolumeInformationByHandleW(
                handle.as_raw_handle(),
                ptr::null_mut(),
                0,
                &mut serial,
                ptr::null_mut(),
                ptr::null_mut(),
                filesystem.as_mut_ptr(),
                filesystem.len() as u32,
            )
        } == 0
        {
            return Err(std_io::Error::last_os_error())
                .context("query opened volume identity and filesystem");
        }
        let fs = String::from_utf16(
            &filesystem[..filesystem
                .iter()
                .position(|&c| c == 0)
                .expect("validated fixed-width native output")],
        )?;
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
        let i64_at = |n| {
            i64::from_le_bytes(
                bytes[n..n + 8]
                    .try_into()
                    .expect("validated fixed-width native output"),
            )
        };
        let maximum_version = if bytes.len() >= 60 {
            u16::from_le_bytes(
                bytes[58..60]
                    .try_into()
                    .expect("validated fixed-width native output"),
            )
        } else {
            2
        };
        ensure!(
            maximum_version >= 2,
            "journal reports unsupported maximum record version {maximum_version}"
        );
        let bounds = Journal {
            id: u64::from_le_bytes(
                bytes[..8]
                    .try_into()
                    .expect("validated fixed-width native output"),
            ),
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
}
