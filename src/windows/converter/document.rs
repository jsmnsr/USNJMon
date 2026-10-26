//! Map schema fields and input provenance to the offline Windows Event XML document.

use super::super::event_log;
use anyhow::Result;
use usnjmon_core::event::Event;
use usnjmon_core::schema::{self, Data};
use usnjmon_evtx::{Element, Value};

fn field<'text>(name: &'text str, value: Value) -> Element<'text> {
    let mut element = Element::new(name);
    element.text = Some(value);
    element
}

pub(super) fn document(
    event: &Event,
    digest: &str,
    offset: u64,
    id: u64,
    timestamp: u64,
) -> Result<Element<'static>> {
    let mut root = Element::new("Event");
    root.attributes.push((
        "xmlns",
        "http://schemas.microsoft.com/win/2004/08/events/event".into(),
    ));
    let mut system = Element::new("System");
    let mut provider = Element::new("Provider");
    provider
        .attributes
        .push(("Name", event_log::PROVIDER.into()));
    let guid = event_log::PROVIDER_ID;
    let mut guid_bytes = [0; 16];
    guid_bytes[..4].copy_from_slice(&guid.data1.to_le_bytes());
    guid_bytes[4..6].copy_from_slice(&guid.data2.to_le_bytes());
    guid_bytes[6..8].copy_from_slice(&guid.data3.to_le_bytes());
    guid_bytes[8..].copy_from_slice(&guid.data4);
    provider.attributes.push(("Guid", Value::Guid(guid_bytes)));
    system.children.push(provider);
    for (name, value) in [
        ("EventID", Value::U16(event.id.try_into()?)),
        ("Version", Value::U8(schema::VERSION)),
        ("Level", Value::U8(4)),
        ("Task", Value::U16(0)),
        ("Opcode", Value::U8(0)),
        ("Keywords", Value::Hex64(1)),
    ] {
        system.children.push(field(name, value));
    }
    let mut time = Element::new("TimeCreated");
    time.attributes
        .push(("SystemTime", Value::Filetime(timestamp)));
    system.children.push(time);
    system.children.push(field("EventRecordID", Value::U64(id)));
    system
        .children
        .push(field("Channel", event_log::LOG.into()));
    system
        .children
        .push(field("Computer", "offline-conversion".into()));
    root.children.push(system);
    let mut data = Element::new("EventData");
    for item in schema::fields(event)? {
        let value = match item.value {
            Data::Text(value) => Value::Text(value),
            Data::U16(value) => Value::U16(value),
            Data::U32(value) => Value::U32(value),
            Data::U64(value) => Value::U64(value),
            Data::I64(value) => Value::I64(value),
            Data::Boolean(value) => Value::Boolean(value),
            Data::Filetime(value) => Value::Filetime(value),
        };
        let mut element = field("Data", value);
        element.attributes.push(("Name", item.name.into()));
        data.children.push(element);
    }
    for (name, value) in [
        ("SourceKind", "extracted_usn_stream".into()),
        ("InputSha256", digest.into()),
        ("InputOffset", Value::U64(offset)),
        ("JournalIdentityStatus", "not_present_in_input".into()),
    ] {
        let mut element = field("Data", value);
        element.attributes.push(("Name", name.into()));
        data.children.push(element);
    }
    root.children.push(data);
    Ok(root)
}
