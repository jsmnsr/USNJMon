use super::volume::wide;
use crate::engine::{self, Event, Sink};
use anyhow::{Context, Result, ensure};
use std::{
    io,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{FreeLibrary, HANDLE},
    System::{
        Diagnostics::Debug::{
            FORMAT_MESSAGE_ARGUMENT_ARRAY, FORMAT_MESSAGE_FROM_HMODULE, FormatMessageW,
        },
        EventLog::{
            DeregisterEventSource, EVENTLOG_ERROR_TYPE, EVENTLOG_INFORMATION_TYPE,
            EVENTLOG_WARNING_TYPE, RegisterEventSourceW, ReportEventW,
        },
        LibraryLoader::{LOAD_LIBRARY_AS_DATAFILE, LoadLibraryExW},
        Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW},
    },
};

pub const SOURCE: &str = "UsnJournalBridge";
pub const LOG: &str = "UsnJournal";
const SOURCE_KEY: &str = r"SYSTEM\CurrentControlSet\Services\EventLog\UsnJournal\UsnJournalBridge";

pub struct EventLog(HANDLE);

impl EventLog {
    pub fn open() -> Result<Self> {
        let dll = registered_message_file()?;
        let expected = std::env::current_exe()?.with_file_name("usn-eventlog-messages.dll");
        ensure!(
            dll.canonicalize()? == expected.canonicalize()?,
            "registered message DLL does not match this installation; reinstall or run the installed executable"
        );
        verify_resources(&dll)?;
        let handle = unsafe { RegisterEventSourceW(null(), wide(SOURCE).as_ptr()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error()).context("RegisterEventSourceW");
        }
        Ok(Self(handle))
    }
}

impl Drop for EventLog {
    fn drop(&mut self) {
        if unsafe { DeregisterEventSource(self.0) } == 0 {
            eprintln!(
                "DeregisterEventSource failed: {}",
                io::Error::last_os_error()
            );
        }
    }
}

fn registered_message_file() -> Result<PathBuf> {
    let mut buffer = [0u16; 32768];
    let mut size = (buffer.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            wide(SOURCE_KEY).as_ptr(),
            wide("EventMessageFile").as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    ensure!(
        status == 0,
        "event source is not correctly registered (Win32 {status}); run Install.ps1; refusing Application-log fallback"
    );
    let length = buffer
        .iter()
        .position(|&v| v == 0)
        .context("unterminated EventMessageFile")?;
    Ok(PathBuf::from(String::from_utf16(&buffer[..length])?))
}

impl Sink for EventLog {
    fn write(&mut self, event: &Event) -> Result<()> {
        let payload = serde_json::to_string(&event.payload)?;
        let string = wide(&payload);
        ensure!(
            string.len() <= 30_000,
            "event payload exceeds safe Event Log insertion size; cursor not advanced"
        );
        let pointers = [string.as_ptr()];
        let level = match event.id {
            engine::GAP | engine::HEALTH => EVENTLOG_WARNING_TYPE,
            engine::FATAL => EVENTLOG_ERROR_TYPE,
            _ => EVENTLOG_INFORMATION_TYPE,
        };
        if unsafe {
            ReportEventW(
                self.0,
                level,
                0,
                event.id,
                null_mut(),
                1,
                0,
                pointers.as_ptr(),
                null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error())
                .context("ReportEventW rejected event; cursor not advanced");
        }
        Ok(())
    }
}

pub fn verify_resources(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let module = unsafe { LoadLibraryExW(path.as_ptr(), null_mut(), LOAD_LIBRARY_AS_DATAFILE) };
    if module.is_null() {
        return Err(io::Error::last_os_error()).context("load message resource DLL");
    }
    let result = (|| {
        let text =
            "{\"filename\":\"literal %1 %%4096 <&> \u{03bb} \u{1f4c1}\",\"schema_version\":1}";
        let insertion = wide(text);
        let arguments = [insertion.as_ptr() as usize];
        for id in 100..=106 {
            let mut buffer = [0u16; 2048];
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
            if length == 0 {
                return Err(io::Error::last_os_error()).context("format event message");
            }
            let rendered = String::from_utf16(&buffer[..length as usize])?;
            ensure!(
                rendered.contains(text),
                "message {id} does not preserve literal insertion content: {rendered}"
            );
        }
        Ok(())
    })();
    unsafe {
        FreeLibrary(module);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn messages_render_without_registration_or_event_writes() {
        verify_resources(Path::new(env!("USN_MESSAGE_DLL"))).unwrap();
    }
}
