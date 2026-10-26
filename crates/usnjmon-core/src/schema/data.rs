//! Named, typed native event fields and their exact wire representations.

use anyhow::{Result, ensure};

#[derive(Debug, PartialEq)]
pub enum Data {
    Text(String),
    U16(u16),
    U32(u32),
    U64(u64),
    I64(i64),
    Boolean(bool),
    Filetime(u64),
}

impl Data {
    #[must_use]
    pub fn input_type(&self) -> &'static str {
        match self {
            Self::Text(_) => "win:UnicodeString",
            Self::U16(_) => "win:UInt16",
            Self::U32(_) => "win:UInt32",
            Self::U64(_) => "win:UInt64",
            Self::I64(_) => "win:Int64",
            Self::Boolean(_) => "win:Boolean",
            Self::Filetime(_) => "win:FILETIME",
        }
    }
    pub fn bytes(&self) -> Result<Vec<u8>> {
        Ok(match self {
            Self::Text(value) => {
                ensure!(
                    !value.contains('\0'),
                    "native event string contains embedded NUL"
                );
                value
                    .encode_utf16()
                    .chain(Some(0))
                    .flat_map(u16::to_le_bytes)
                    .collect()
            }
            Self::U16(v) => v.to_le_bytes().to_vec(),
            Self::U32(v) => v.to_le_bytes().to_vec(),
            Self::U64(v) | Self::Filetime(v) => v.to_le_bytes().to_vec(),
            Self::I64(v) => v.to_le_bytes().to_vec(),
            Self::Boolean(v) => u32::from(*v).to_le_bytes().to_vec(),
        })
    }
}

pub struct Field {
    pub name: &'static str,
    pub value: Data,
}

impl Field {
    pub(super) fn new(name: &'static str, value: Data) -> Self {
        Self { name, value }
    }
}
