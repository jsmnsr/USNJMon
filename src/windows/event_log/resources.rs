//! In-memory provider resource verification; never registers or writes events.

use super::super::wide;
use anyhow::{Context, Result, ensure};
use std::path::Path;
use std::{io, ptr};
use windows_sys::Win32::Foundation::FreeLibrary;
use windows_sys::Win32::System::Diagnostics::Debug::{
    FORMAT_MESSAGE_ARGUMENT_ARRAY, FORMAT_MESSAGE_FROM_HMODULE, FormatMessageW,
};
use windows_sys::Win32::System::LibraryLoader::{LOAD_LIBRARY_AS_DATAFILE, LoadLibraryExW};
const HEADER: &str = include_str!(env!("USN_PROVIDER_HEADER"));

pub fn verify_resources(path: &Path) -> Result<()> {
    let ids: Vec<_> = HEADER
        .lines()
        .filter(|line| line.starts_with("#define MSG_"))
        .map(|line| -> Result<u32> {
            let value = line
                .split_whitespace()
                .nth(2)
                .context("missing generated message ID")?;
            Ok(u32::from_str_radix(
                value.trim_end_matches('L').trim_start_matches("0x"),
                16,
            )?)
        })
        .collect::<Result<_>>()?;
    ensure!(
        !ids.is_empty(),
        "manifest compiler generated no message IDs"
    );
    format_resources(path, &ids)
}

fn format_resources(path: &Path, ids: &[u32]) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: Path is terminated; DATAFILE maps resources without executing DLL initialization.
    let module =
        unsafe { LoadLibraryExW(path.as_ptr(), ptr::null_mut(), LOAD_LIBRARY_AS_DATAFILE) };
    ensure!(
        !module.is_null(),
        "load message resource DLL: {}",
        io::Error::last_os_error()
    );
    let result = (|| {
        let sentinel = "literal %1 %%4096 <&> \u{03bb} \u{1f4c1}";
        let insertion = wide(sentinel);
        let arguments = [insertion.as_ptr() as usize; 27];
        for &id in ids {
            let mut buffer = [0u16; 8192];
            // SAFETY: Resource module is live; argument pointers and output buffer stay valid.
            let length = unsafe {
                FormatMessageW(
                    FORMAT_MESSAGE_FROM_HMODULE | FORMAT_MESSAGE_ARGUMENT_ARRAY,
                    module.cast(),
                    id,
                    0x409,
                    buffer.as_mut_ptr(),
                    buffer.len() as u32,
                    arguments.as_ptr().cast(),
                )
            };
            ensure!(
                length != 0,
                "format message {id:#x}: {}",
                io::Error::last_os_error()
            );
            let rendered = String::from_utf16(&buffer[..length as usize])?;
            ensure!(
                rendered.contains(sentinel),
                "message {id:#x} did not preserve literal insertions"
            );
            ensure!(
                rendered.contains('\n'),
                "native description must contain readable line breaks"
            );
        }
        Ok(())
    })();
    // SAFETY: This function exclusively owns the loaded resource module.
    unsafe {
        FreeLibrary(module);
    }
    result
}
