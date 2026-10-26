//! Verify installed provider paths and fail closed when its channel is disabled.

use super::super::wide;
use super::{LOG, PROVIDER_KEY};
use anyhow::{Context, Result, ensure};
use std::path::PathBuf;
use std::{io, mem, ptr};
use windows_sys::Win32::System::EventLog::{
    EVT_VARIANT, EvtChannelConfigEnabled, EvtClose, EvtGetChannelConfigProperty,
    EvtOpenChannelConfig, EvtVarTypeBoolean,
};
use windows_sys::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW,
};

pub(super) fn registered_file(property: &str) -> Result<PathBuf> {
    let mut buffer = [0u16; 32768];
    let mut size = (buffer.len() * 2) as u32;
    // SAFETY: Names are terminated; buffer is aligned and size is its byte capacity.
    let code = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            wide(PROVIDER_KEY).as_ptr(),
            wide(property).as_ptr(),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
            ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    ensure!(
        code == 0,
        "native provider not correctly registered (Win32 {code}); run Install.ps1 or Install.ps1 -Upgrade"
    );
    let length = buffer
        .iter()
        .position(|&v| v == 0)
        .context("unterminated provider resource path")?;
    Ok(PathBuf::from(String::from_utf16(&buffer[..length])?))
}

pub(super) fn channel_enabled() -> Result<()> {
    // SAFETY: The channel name is NUL-terminated and remains live during the call.
    let handle = unsafe { EvtOpenChannelConfig(0, wide(LOG).as_ptr(), 0) };
    ensure!(
        handle != 0,
        "cannot open {LOG}: {}",
        io::Error::last_os_error()
    );
    // SAFETY: EVT_VARIANT is a C integer/pointer union with a valid all-zero representation.
    let mut value: EVT_VARIANT = unsafe { mem::zeroed() };
    let mut used = 0;
    // SAFETY: Handle is open; the aligned output has exactly the supplied byte capacity.
    let success = unsafe {
        EvtGetChannelConfigProperty(
            handle,
            EvtChannelConfigEnabled,
            0,
            mem::size_of::<EVT_VARIANT>() as u32,
            &mut value,
            &mut used,
        )
    };
    let error = if success == 0 {
        Some(io::Error::last_os_error())
    } else {
        None
    };
    // SAFETY: This function exclusively owns the successful channel configuration handle.
    unsafe {
        EvtClose(handle);
    }
    if let Some(error) = error {
        return Err(error).context("query channel enablement");
    }
    ensure!(
        // SAFETY: The short-circuit type guard selects the BooleanVal union member.
        value.Type == EvtVarTypeBoolean as u32 && unsafe { value.Anonymous.BooleanVal } != 0,
        "{LOG} is disabled; refusing silent ETW discard"
    );
    Ok(())
}
