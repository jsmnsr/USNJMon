//! Schema-2 native fields shared by the live ETW adapter and offline conversion.
mod category;
mod data;
mod notice;
mod record;
mod values;

pub use category::category;
pub use data::{Data, Field};

use crate::event::Event;
use anyhow::Result;

pub const VERSION: u8 = 2;
pub const RECORD_IDS: [u32; 8] = [1000, 1001, 1002, 1003, 1010, 1020, 1030, 1099];

pub fn fields(event: &Event) -> Result<Vec<Field>> {
    let mut fields = vec![Field::new("SchemaVersion", Data::U32(VERSION.into()))];
    if RECORD_IDS.contains(&event.id) {
        record::append(event, &mut fields)?;
    } else {
        notice::append(event, &mut fields)?;
    }
    Ok(fields)
}

#[cfg(test)]
mod tests;
