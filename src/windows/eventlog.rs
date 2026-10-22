use super::volume::wide;
use crate::{
    engine::{self, Event, Sink},
    schema,
    stop::Stop,
};
use anyhow::{Context, Result, ensure};
use std::{
    io,
    mem::{size_of, zeroed},
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    time::Duration,
};
use windows_sys::{
    Win32::{
        Foundation::FreeLibrary,
        System::{
            Diagnostics::{
                Debug::{
                    FORMAT_MESSAGE_ARGUMENT_ARRAY, FORMAT_MESSAGE_FROM_HMODULE, FormatMessageW,
                },
                Etw::{
                    EVENT_DATA_DESCRIPTOR, EVENT_DESCRIPTOR, EventEnabled, EventRegister,
                    EventUnregister, EventWrite, REGHANDLE,
                },
            },
            EventLog::{
                EVT_VARIANT, EvtChannelConfigEnabled, EvtClose, EvtGetChannelConfigProperty,
                EvtOpenChannelConfig, EvtVarTypeBoolean,
            },
            LibraryLoader::{LOAD_LIBRARY_AS_DATAFILE, LoadLibraryExW},
            Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW},
        },
    },
    core::GUID,
};

pub const PROVIDER: &str = "UsnEventLog";
pub const LOG: &str = "UsnEventLog/Operational";
const PROVIDER_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\WINEVT\Publishers\{4fbeac1d-5fe8-44c1-ab4e-a06a645bf2d8}";
const PROVIDER_ID: GUID = GUID {
    data1: 0x4fbeac1d,
    data2: 0x5fe8,
    data3: 0x44c1,
    data4: [0xab, 0x4e, 0xa0, 0x6a, 0x64, 0x5b, 0xf2, 0xd8],
};
const HEADER: &str = include_str!(env!("USN_PROVIDER_HEADER"));

pub struct EventLog(REGHANDLE);

fn descriptor(id: u32) -> Result<EVENT_DESCRIPTOR> {
    ensure!(
        schema::RECORD_IDS.contains(&id) || (101..=107).contains(&id),
        "unknown event ID {id}"
    );
    Ok(EVENT_DESCRIPTOR {
        Id: id.try_into()?,
        Version: schema::VERSION,
        Channel: 16,
        Level: match id {
            engine::GAP | engine::HEALTH => 3,
            engine::FATAL => 2,
            _ => 4,
        },
        Opcode: 0,
        Task: 0,
        Keyword: 0x8000000000000000
            | if schema::RECORD_IDS.contains(&id) {
                1
            } else {
                2
            },
    })
}

impl EventLog {
    pub fn open(stop: &Stop) -> Result<Self> {
        let expected = std::env::current_exe()?
            .with_file_name("usn-eventlog-provider.dll")
            .canonicalize()?;
        for property in ["MessageFileName", "ResourceFileName"] {
            ensure!(
                registered_file(property)?.canonicalize()? == expected,
                "registered {property} differs from this installation; run Install.ps1 -Upgrade"
            );
        }
        verify_resources(&expected)?;
        channel_enabled()?;
        let mut handle = 0;
        let code = unsafe { EventRegister(&PROVIDER_ID, None, null(), &mut handle) };
        ensure!(
            code == 0,
            "EventRegister failed: {}",
            io::Error::from_raw_os_error(code as i32)
        );
        let sink = Self(handle);
        let event = descriptor(engine::LIFECYCLE)?;
        for _ in 0..50 {
            if unsafe { EventEnabled(handle, &event) } {
                return Ok(sink);
            }
            ensure!(
                !stop.wait(Duration::from_millis(100)),
                "provider initialization cancelled"
            );
        }
        anyhow::bail!("Event Log did not enable {PROVIDER}; no checkpoints advanced")
    }
}

impl Drop for EventLog {
    fn drop(&mut self) {
        let code = unsafe { EventUnregister(self.0) };
        if code != 0 {
            eprintln!("EventUnregister failed: {code:#x}");
        }
    }
}

fn registered_file(property: &str) -> Result<PathBuf> {
    let mut buffer = [0u16; 32768];
    let mut size = (buffer.len() * 2) as u32;
    let code = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            wide(PROVIDER_KEY).as_ptr(),
            wide(property).as_ptr(),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
            null_mut(),
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

fn channel_enabled() -> Result<()> {
    let handle = unsafe { EvtOpenChannelConfig(0, wide(LOG).as_ptr(), 0) };
    ensure!(
        handle != 0,
        "cannot open {LOG}: {}",
        io::Error::last_os_error()
    );
    let mut value: EVT_VARIANT = unsafe { zeroed() };
    let mut used = 0;
    let success = unsafe {
        EvtGetChannelConfigProperty(
            handle,
            EvtChannelConfigEnabled,
            0,
            size_of::<EVT_VARIANT>() as u32,
            &mut value,
            &mut used,
        )
    };
    let error = if success == 0 {
        Some(io::Error::last_os_error())
    } else {
        None
    };
    unsafe {
        EvtClose(handle);
    }
    if let Some(error) = error {
        return Err(error).context("query channel enablement");
    }
    ensure!(
        value.Type == EvtVarTypeBoolean as u32 && unsafe { value.Anonymous.BooleanVal } != 0,
        "{LOG} is disabled; refusing silent ETW discard"
    );
    Ok(())
}

fn checked_write(mut ready: impl FnMut() -> Result<()>, write: impl FnOnce() -> u32) -> Result<()> {
    ready()?;
    let code = write();
    ensure!(
        code == 0,
        "EventWrite rejected event ({code:#x}): {}; checkpoint not advanced",
        io::Error::from_raw_os_error(code as i32)
    );
    // ETW success alone includes the disabled-provider case. Administrative races
    // still cannot be made transactional with delivery and checkpoint persistence.
    ready()
}

impl Sink for EventLog {
    fn write(&mut self, event: &Event) -> Result<()> {
        let event_descriptor = descriptor(event.id)?;
        let values = schema::fields(event)?
            .into_iter()
            .map(|field| field.1.bytes())
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            values.iter().map(Vec::len).sum::<usize>() <= 48 * 1024,
            "native event exceeds 48 KiB safe payload limit"
        );
        let descriptors: Vec<_> = values
            .iter()
            .map(|value| {
                let mut data: EVENT_DATA_DESCRIPTOR = unsafe { zeroed() };
                data.Ptr = value.as_ptr() as u64;
                data.Size = value.len() as u32;
                data
            })
            .collect();
        checked_write(
            || {
                channel_enabled()?;
                ensure!(
                    unsafe { EventEnabled(self.0, &event_descriptor) },
                    "native event {} is not enabled; refusing silent ETW discard",
                    event.id
                );
                Ok(())
            },
            || unsafe {
                EventWrite(
                    self.0,
                    &event_descriptor,
                    descriptors.len() as u32,
                    descriptors.as_ptr(),
                )
            },
        )
    }
}

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
    format_resources(path, &ids, true)
}

fn format_resources(path: &Path, ids: &[u32], native: bool) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let module = unsafe { LoadLibraryExW(path.as_ptr(), null_mut(), LOAD_LIBRARY_AS_DATAFILE) };
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
            if native {
                ensure!(
                    rendered.contains('\n'),
                    "native description must contain readable line breaks"
                );
            }
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
    use std::cell::Cell;

    #[test]
    fn legacy_and_native_resources_render_without_registration() {
        format_resources(
            Path::new(env!("USN_MESSAGE_DLL")),
            &(100..=106).collect::<Vec<_>>(),
            false,
        )
        .unwrap();
        verify_resources(Path::new(env!("USN_PROVIDER_DLL"))).unwrap();
    }

    #[test]
    fn disabled_provider_and_write_errors_fail_closed() {
        let written = Cell::new(false);
        assert!(
            checked_write(
                || anyhow::bail!("disabled"),
                || {
                    written.set(true);
                    0
                }
            )
            .is_err()
        );
        assert!(!written.get());
        assert!(checked_write(|| Ok(()), || 8).is_err());
        let checks = Cell::new(0);
        assert!(
            checked_write(
                || {
                    checks.set(checks.get() + 1);
                    ensure!(checks.get() == 1, "disabled after acceptance");
                    Ok(())
                },
                || 0
            )
            .is_err()
        );
        assert!(checked_write(|| Ok(()), || 0).is_ok());
    }

    #[test]
    fn descriptors_match_manifest_compiler_output() {
        let header: String = HEADER.chars().filter(|c| !c.is_whitespace()).collect();
        for id in schema::RECORD_IDS.into_iter().chain(101..=107) {
            let d = descriptor(id).unwrap();
            let expected = format!(
                "={{0x{:x},0x{:x},0x{:x},0x{:x},0x{:x},0x{:x},0x{:x}}}",
                d.Id, d.Version, d.Channel, d.Level, d.Opcode, d.Task, d.Keyword
            );
            assert!(
                header.to_ascii_lowercase().contains(&expected),
                "{id}: {expected}"
            );
        }
    }
}
