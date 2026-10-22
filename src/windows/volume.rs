//! Read-only native volume I/O with safe buffer lifetimes through completion/cancellation.
//! FFI is required for USN controls and file-ID lookup, neither of which std exposes.

use crate::engine::{Event, Journal};
use crate::filter::ResolvedPath;
use crate::journal::BUFFER_SIZE;
use crate::stop::Stop;
use anyhow::{Context, Result, ensure};
use std::ffi::c_void;
use std::io;
use std::mem;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr;
use windows_sys::Win32::Foundation::{
    ERROR_IO_PENDING, ERROR_NOT_FOUND, ERROR_OPERATION_ABORTED, GENERIC_READ, INVALID_HANDLE_VALUE,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ExtendedFileIdType, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_FLAG_OVERLAPPED, FILE_ID_128, FILE_ID_DESCRIPTOR, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FileIdType, GetFinalPathNameByHandleW, GetVolumeInformationByHandleW,
    OPEN_EXISTING, OpenFileById, VOLUME_NAME_GUID,
};
use windows_sys::Win32::System::IO::{
    CancelIoEx, DeviceIoControl, GetOverlappedResult, OVERLAPPED,
};
use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

const FSCTL_QUERY_USN_JOURNAL: u32 = 0x000900f4;
const FSCTL_READ_USN_JOURNAL: u32 = 0x000900bb;

#[must_use]
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
            return Err(io::Error::last_os_error())
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
            return Err(io::Error::last_os_error())
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

    pub fn resolve_record_path(&self, event: &Event) -> Result<ResolvedPath> {
        let record = &event.payload["record"];
        let parent = record["parent_reference"]
            .as_str()
            .context("missing parent reference")?;
        let filename = record["filename"].as_str().context("missing filename")?;
        if record["filename_lossy"].as_bool() != Some(false)
            || filename.is_empty()
            || filename == "."
            || filename == ".."
            || filename.chars().any(|c| {
                c.is_control() || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
            })
        {
            return Ok(ResolvedPath::unresolved("unrepresentable_filename"));
        }
        let descriptor = Self::parent_descriptor(parent)?;
        // SAFETY: Handle is live and descriptor tag matches the initialized union member.
        let raw = unsafe {
            OpenFileById(
                self.handle.as_raw_handle(),
                &descriptor,
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null(),
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Ok(ResolvedPath::unresolved(format!(
                "parent_open_win32_{}",
                io::Error::last_os_error().raw_os_error().unwrap_or(0)
            )));
        }
        // SAFETY: OpenFileById succeeded and ownership is transferred exactly once.
        let parent_handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut buffer = vec![0u16; 32768];
        // SAFETY: Handle is live; buffer has the stated WCHAR capacity.
        let length = unsafe {
            GetFinalPathNameByHandleW(
                parent_handle.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                VOLUME_NAME_GUID,
            )
        };
        if length == 0 {
            return Ok(ResolvedPath::unresolved(format!(
                "parent_path_win32_{}",
                io::Error::last_os_error().raw_os_error().unwrap_or(0)
            )));
        }
        if length as usize >= buffer.len() {
            return Ok(ResolvedPath::unresolved("parent_path_too_long"));
        }
        let Ok(parent_path) = String::from_utf16(&buffer[..length as usize]) else {
            return Ok(ResolvedPath::unresolved("parent_path_invalid_utf16"));
        };
        Self::candidate_path(&self.identity, &parent_path, filename)
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

    fn parent_descriptor(reference: &str) -> Result<FILE_ID_DESCRIPTOR> {
        // SAFETY: This C integer union has a valid zero representation; tag/member are set below.
        let mut descriptor: FILE_ID_DESCRIPTOR = unsafe { mem::zeroed() };
        descriptor.dwSize = mem::size_of::<FILE_ID_DESCRIPTOR>() as u32;
        match reference.len() {
            16 => {
                descriptor.Type = FileIdType;
                descriptor.Anonymous.FileId = u64::from_str_radix(reference, 16)? as i64;
            }
            32 => {
                descriptor.Type = ExtendedFileIdType;
                descriptor.Anonymous.ExtendedFileId = FILE_ID_128 {
                    Identifier: u128::from_str_radix(reference, 16)?.to_le_bytes(),
                };
            }
            _ => anyhow::bail!("invalid parent reference length"),
        }
        Ok(descriptor)
    }

    fn candidate_path(identity: &str, parent: &str, filename: &str) -> Result<ResolvedPath> {
        let root = identity
            .rsplit_once(':')
            .context("invalid volume identity")?
            .0;
        if !parent
            .get(..root.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(root))
        {
            return Ok(ResolvedPath::unresolved("parent_volume_mismatch"));
        }
        let relative_parent = &parent[root.len()..];
        if !relative_parent.starts_with('\\') {
            return Ok(ResolvedPath::unresolved("parent_volume_boundary"));
        }
        let relative = format!("{}\\{filename}", relative_parent.trim_end_matches('\\'));
        if crate::filter::normalize_path(&relative).is_err()
            || relative.encode_utf16().count() > 4096
        {
            return Ok(ResolvedPath::unresolved(
                "parent_path_unrepresentable_or_too_long",
            ));
        }
        Ok(ResolvedPath {
            full: Some(format!("{root}{relative}")),
            relative: Some(relative),
            status: "parent_current_only".into(),
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

    #[test]
    fn parent_ids_and_candidate_paths_preserve_record_names_without_claiming_history() {
        let id = Volume::parent_descriptor("0001000000000042").unwrap();
        assert_eq!(id.Type, FileIdType);
        // SAFETY: The verified tag selects the FileId union member initialized by parent_descriptor.
        assert_eq!(unsafe { id.Anonymous.FileId }, 0x0001000000000042);
        let id = Volume::parent_descriptor("00000000000000010000000000000042").unwrap();
        assert_eq!(id.Type, ExtendedFileIdType);
        assert_eq!(
            // SAFETY: The verified tag selects the initialized ExtendedFileId member.
            unsafe { id.Anonymous.ExtendedFileId.Identifier },
            0x10000000000000042u128.to_le_bytes()
        );
        assert!(Volume::parent_descriptor("invalid").is_err());
        let root = r"\\?\volume{00000000-0000-0000-0000-000000000001}";
        let identity = format!("{root}:12345678");
        let path = Volume::candidate_path(
            &identity,
            &format!("{root}\\renamed-parent"),
            "old-name.txt",
        )
        .unwrap();
        assert_eq!(
            path.relative.as_deref(),
            Some(r"\renamed-parent\old-name.txt")
        );
        assert_eq!(path.status, "parent_current_only");
        assert!(
            Volume::candidate_path(&identity, r"\\?\Volume{other}\parent", "file")
                .unwrap()
                .relative
                .is_none()
        );
        assert!(
            Volume::candidate_path(&identity, &format!("{root}suffix"), "file")
                .unwrap()
                .relative
                .is_none()
        );
        let path = Volume::candidate_path(&identity, &format!("{root}\\"), "deleted.txt").unwrap();
        assert_eq!(path.relative.as_deref(), Some(r"\deleted.txt"));
    }

    #[test]
    fn resolves_only_an_owned_fixture_directory_by_id_without_reading_a_journal() {
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let path = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("parent-resolution-{}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(&path)
            .unwrap();
        // SAFETY: This C integer-only output structure has a valid zero representation.
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { mem::zeroed() };
        assert_ne!(
            // SAFETY: Owned fixture handle and aligned output storage remain live.
            unsafe { GetFileInformationByHandle(directory.as_raw_handle(), &mut info) },
            0
        );
        let reference = format!(
            "{:016x}",
            ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64
        );
        let mut buffer = [0u16; 4096];
        // SAFETY: Owned fixture handle is live; buffer has the stated WCHAR capacity.
        let size = unsafe {
            GetFinalPathNameByHandleW(
                directory.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                VOLUME_NAME_GUID,
            )
        };
        assert!(size > 0 && (size as usize) < buffer.len());
        let full = String::from_utf16(&buffer[..size as usize]).unwrap();
        let end = full.find('}').unwrap() + 1;
        let fixture = Volume {
            handle: directory.into(),
            identity: format!(
                "{}:{:08x}",
                full[..end].to_ascii_lowercase(),
                info.dwVolumeSerialNumber
            ),
        };
        let event = Event::new(
            1000,
            serde_json::json!({"record":{
                "parent_reference": reference, "filename":"already-deleted.txt", "filename_lossy":false
            }}),
        );
        let resolved = fixture.resolve_record_path(&event);
        drop(fixture);
        std::fs::remove_dir(path).unwrap();
        let resolved = resolved.unwrap();
        assert_eq!(resolved.status, "parent_current_only");
        assert!(resolved.full.unwrap().ends_with(r"\already-deleted.txt"));
    }
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
