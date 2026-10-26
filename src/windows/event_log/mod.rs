//! Own ETW registration and synchronously deliver stable native field buffers.

mod registration;
mod resources;
use anyhow::{Result, ensure};
use registration::{channel_enabled, registered_file};
pub use resources::verify_resources;
use std::time::Duration;
use std::{io, mem, ptr};
use usnjmon_core::event::{self, Event, Sink};
use usnjmon_core::schema;
use usnjmon_core::stop::Stop;
use windows_sys::Win32::System::Diagnostics::Etw::{
    EVENT_DATA_DESCRIPTOR, EVENT_DESCRIPTOR, EventEnabled, EventRegister, EventUnregister,
    EventWrite, REGHANDLE,
};
use windows_sys::core::GUID;

pub const PROVIDER: &str = "USNJMon";
pub const PROVIDER_GUID: &str = "{6427DBF2-7731-48EA-9166-4A9CF0A3B5D7}";
pub const LOG: &str = "USNJMon/Operational";
const PROVIDER_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\WINEVT\Publishers\{6427dbf2-7731-48ea-9166-4a9cf0a3b5d7}";
pub(crate) const PROVIDER_ID: GUID = GUID {
    data1: 0x6427dbf2,
    data2: 0x7731,
    data3: 0x48ea,
    data4: [0x91, 0x66, 0x4a, 0x9c, 0xf0, 0xa3, 0xb5, 0xd7],
};

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
            event::GAP | event::HEALTH => 3,
            event::FATAL => 2,
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
            .with_file_name("usnjmon-provider.dll")
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
        // SAFETY: GUID and output handle are valid; no callback or context is retained.
        let code = unsafe { EventRegister(&PROVIDER_ID, None, ptr::null(), &mut handle) };
        ensure!(
            code == 0,
            "EventRegister failed: {}",
            io::Error::from_raw_os_error(code as i32)
        );
        let sink = Self(handle);
        let event = descriptor(event::LIFECYCLE)?;
        for _ in 0..50 {
            // SAFETY: Registration remains owned by sink and descriptor is initialized.
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
        // SAFETY: Self exclusively owns this successful EventRegister registration.
        let code = unsafe { EventUnregister(self.0) };
        if code != 0 {
            eprintln!("EventUnregister failed: {code:#x}");
        }
    }
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
            .map(|field| field.value.bytes())
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            values.iter().map(Vec::len).sum::<usize>() <= 48 * 1024,
            "native event exceeds 48 KiB safe payload limit"
        );
        let descriptors: Vec<_> = values
            .iter()
            .map(|value| {
                // SAFETY: This integer-only descriptor has a valid all-zero representation.
                let mut data: EVENT_DATA_DESCRIPTOR = unsafe { mem::zeroed() };
                data.Ptr = value.as_ptr() as u64;
                data.Size = value.len() as u32;
                data
            })
            .collect();
        checked_write(
            || {
                channel_enabled()?;
                ensure!(
                    // SAFETY: Registration and fully initialized descriptor remain live.
                    unsafe { EventEnabled(self.0, &event_descriptor) },
                    "native event {} is not enabled; refusing silent ETW discard",
                    event.id
                );
                Ok(())
            },
            // SAFETY: Descriptors point to stable, live buffers until synchronous EventWrite returns.
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

#[cfg(test)]
mod tests;
