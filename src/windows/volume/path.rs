//! Resolve a current parent by file ID without claiming a historical full path.

use super::Volume;
use anyhow::{Context, Result};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::{io, mem, ptr};
use usnjmon_core::event::Event;
use usnjmon_core::filter::ResolvedPath;
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Storage::FileSystem::{
    ExtendedFileIdType, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_128,
    FILE_ID_DESCRIPTOR, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdType,
    GetFinalPathNameByHandleW, OpenFileById, VOLUME_NAME_GUID,
};

impl Volume {
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
        if usnjmon_core::filter::normalize_path(&relative).is_err()
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
}
