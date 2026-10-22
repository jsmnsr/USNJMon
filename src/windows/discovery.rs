use super::volume::wide;
use crate::config::{DiscoveredVolume, VolumeConfig, Volumes, volume_key};
use anyhow::{Context, Result, ensure};
use std::{collections::HashSet, io, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::{ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{
        FindFirstVolumeW, FindNextVolumeW, FindVolumeClose, GetVolumeInformationW,
        GetVolumeNameForVolumeMountPointW, GetVolumePathNamesForVolumeNameW,
    },
};

pub fn resolve_root(root: &str) -> Result<String> {
    let normalized = volume_key(root)?;
    if normalized.starts_with(r"\\?\volume{") {
        return Ok(normalized);
    }
    let mut buffer = [0u16; 128];
    ensure!(
        unsafe {
            GetVolumeNameForVolumeMountPointW(
                wide(&normalized).as_ptr(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
            )
        } != 0,
        "resolve volume mount point {root}: {}",
        io::Error::last_os_error()
    );
    volume_key(&terminated(&buffer)?)
}

fn terminated(buffer: &[u16]) -> Result<String> {
    let end = buffer
        .iter()
        .position(|&unit| unit == 0)
        .context("unterminated volume path")?;
    Ok(String::from_utf16(&buffer[..end])?)
}

struct Search(HANDLE);
impl Drop for Search {
    fn drop(&mut self) {
        if unsafe { FindVolumeClose(self.0) } == 0 {
            eprintln!("FindVolumeClose: {}", io::Error::last_os_error());
        }
    }
}

pub struct Selection {
    pub volumes: Vec<VolumeConfig>,
    pub warnings: Vec<(String, String)>,
}

pub fn select(config: &Volumes) -> Result<Selection> {
    if let Volumes::Explicit(volumes) = config {
        let mut identities = HashSet::new();
        let mut warnings = Vec::new();
        for volume in volumes {
            match resolve_root(&volume.path) {
                Ok(root) => ensure!(
                    identities.insert(root),
                    "explicit volume aliases select the same volume more than once"
                ),
                Err(error) => warnings.push((
                    volume.path.clone(),
                    format!("{error:#}; explicit worker will retry"),
                )),
            }
        }
        return Ok(Selection {
            volumes: volumes.clone(),
            warnings,
        });
    }
    let Volumes::Discover(selectors) = config else {
        unreachable!()
    };
    let mut name = [0u16; 128];
    let handle = unsafe { FindFirstVolumeW(name.as_mut_ptr(), name.len() as u32) };
    ensure!(
        handle != INVALID_HANDLE_VALUE,
        "enumerate local volumes: {}",
        io::Error::last_os_error()
    );
    let search = Search(handle);
    let mut discovered = Vec::new();
    let mut warnings = Vec::new();
    loop {
        let path = terminated(&name)?;
        let mut filesystem = [0u16; 32];
        let success = unsafe {
            GetVolumeInformationW(
                wide(&path).as_ptr(),
                null_mut(),
                0,
                null_mut(),
                null_mut(),
                null_mut(),
                filesystem.as_mut_ptr(),
                filesystem.len() as u32,
            )
        };
        if success == 0 {
            warnings.push((
                path,
                format!(
                    "volume discovery could not query filesystem: {}",
                    io::Error::last_os_error()
                ),
            ));
        } else if terminated(&filesystem)? == "NTFS" {
            let mut mounts = vec![0u16; 32768];
            let mut used = 0;
            let success = unsafe {
                GetVolumePathNamesForVolumeNameW(
                    wide(&path).as_ptr(),
                    mounts.as_mut_ptr(),
                    mounts.len() as u32,
                    &mut used,
                )
            };
            let aliases = if success == 0 {
                warnings.push((
                    path.clone(),
                    format!(
                        "volume mount aliases unavailable; GUID remains selectable: {}",
                        io::Error::last_os_error()
                    ),
                ));
                Vec::new()
            } else {
                ensure!(
                    used as usize <= mounts.len(),
                    "invalid mount-path buffer length"
                );
                mounts[..used as usize]
                    .split(|&unit| unit == 0)
                    .filter(|part| !part.is_empty())
                    .map(String::from_utf16)
                    .collect::<std::result::Result<Vec<_>, _>>()?
            };
            discovered.push(DiscoveredVolume {
                path,
                mounts: aliases,
            });
        }
        if unsafe { FindNextVolumeW(search.0, name.as_mut_ptr(), name.len() as u32) } == 0 {
            let error = io::Error::last_os_error();
            ensure!(
                error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32),
                "continue volume discovery: {error}"
            );
            break;
        }
    }
    // Emit discovery warnings even if an explicit inclusion later fails selection.
    for (path, error) in &warnings {
        eprintln!("{path}: {error}");
    }
    Ok(Selection {
        volumes: selectors.select(discovered)?,
        warnings,
    })
}
