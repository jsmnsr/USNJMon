//! Typed template substitutions with chunk-relative name and template offsets.

use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};

pub struct Element<'text> {
    pub name: &'text str,
    pub attributes: Vec<(&'text str, Value)>,
    pub text: Option<Value>,
    pub children: Vec<Element<'text>>,
}

pub enum Value {
    Text(String),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    I64(i64),
    Boolean(bool),
    Filetime(u64),
    Guid([u8; 16]),
    Hex64(u64),
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self::Text(value.into())
    }
}

impl Value {
    fn encoded(&self) -> (u8, Vec<u8>) {
        match self {
            Self::Text(value) => (
                1,
                value
                    .encode_utf16()
                    .chain(Some(0))
                    .flat_map(u16::to_le_bytes)
                    .collect(),
            ),
            Self::U8(value) => (4, vec![*value]),
            Self::U16(value) => (6, value.to_le_bytes().to_vec()),
            Self::U32(value) => (8, value.to_le_bytes().to_vec()),
            Self::I64(value) => (9, value.to_le_bytes().to_vec()),
            Self::U64(value) => (10, value.to_le_bytes().to_vec()),
            Self::Boolean(value) => (13, u32::from(*value).to_le_bytes().to_vec()),
            Self::Guid(value) => (15, value.to_vec()),
            Self::Filetime(value) => (17, value.to_le_bytes().to_vec()),
            Self::Hex64(value) => (21, value.to_le_bytes().to_vec()),
        }
    }
}

pub(super) struct Fragment {
    pub(super) bytes: Vec<u8>,
    pub(super) names: Vec<(u16, usize)>,
    pub(super) template_id: u32,
}

impl<'text> Element<'text> {
    #[must_use]
    pub fn new(name: &'text str) -> Self {
        Self {
            name,
            attributes: Vec::new(),
            text: None,
            children: Vec::new(),
        }
    }
}

pub(super) fn encode(element: &Element<'_>, base: usize) -> Result<Fragment> {
    let mut bytes = vec![0x0f, 1, 1, 0, 0x0c, 1];
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&((base + 14) as u32).to_le_bytes());
    bytes.extend_from_slice(&[0; 24]);
    let mut names = Vec::new();
    let mut values = Vec::new();
    bytes.extend_from_slice(&[0x0f, 1, 1, 0]);
    encode_element(element, &mut bytes, base, &mut names, &mut values)?;
    bytes.push(0);
    let length = (bytes.len() - 38) as u32;
    let digest = Sha256::digest(&bytes[38..]);
    let template_id = u32::from_le_bytes(digest[..4].try_into().expect("four-byte template ID"));
    bytes[6..10].copy_from_slice(&template_id.to_le_bytes());
    bytes[18..34].copy_from_slice(&digest[..16]);
    bytes[34..38].copy_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(&(values.len() as u32).to_le_bytes());
    for (kind, value) in &values {
        bytes.extend_from_slice(&u16::try_from(value.len())?.to_le_bytes());
        bytes.extend_from_slice(&[*kind, 0]);
    }
    for (_, value) in values {
        bytes.extend_from_slice(&value);
    }
    bytes.push(0);
    Ok(Fragment {
        bytes,
        names,
        template_id,
    })
}

fn name(
    value: &str,
    bytes: &mut Vec<u8>,
    base: usize,
    names: &mut Vec<(u16, usize)>,
) -> Result<()> {
    let units: Vec<u16> = value.encode_utf16().collect();
    let offset = base + bytes.len() + 4;
    bytes.extend_from_slice(&(offset as u32).to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    let hash = units.iter().fold(0u16, |hash, unit| {
        hash.wrapping_mul(65599u32 as u16).wrapping_add(*unit)
    });
    names.push((hash, offset));
    bytes.extend_from_slice(&hash.to_le_bytes());
    bytes.extend_from_slice(&u16::try_from(units.len())?.to_le_bytes());
    for unit in units {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes.extend_from_slice(&0u16.to_le_bytes());
    Ok(())
}

fn substitution(value: &Value, bytes: &mut Vec<u8>, values: &mut Vec<(u8, Vec<u8>)>) -> Result<()> {
    let index = u16::try_from(values.len())?;
    let encoded = value.encoded();
    ensure!(
        encoded.1.len() <= u16::MAX as usize,
        "EVTX value exceeds substitution limit"
    );
    bytes.push(0x0d);
    bytes.extend_from_slice(&index.to_le_bytes());
    bytes.push(encoded.0);
    values.push(encoded);
    Ok(())
}

fn encode_element(
    element: &Element<'_>,
    bytes: &mut Vec<u8>,
    base: usize,
    names: &mut Vec<(u16, usize)>,
    values: &mut Vec<(u8, Vec<u8>)>,
) -> Result<()> {
    bytes.push(if element.attributes.is_empty() {
        1
    } else {
        0x41
    });
    bytes.extend_from_slice(&u16::MAX.to_le_bytes());
    let size_offset = bytes.len();
    bytes.extend_from_slice(&0u32.to_le_bytes());
    name(element.name, bytes, base, names)?;
    if !element.attributes.is_empty() {
        let attribute_size = bytes.len();
        bytes.extend_from_slice(&0u32.to_le_bytes());
        for (index, (key, value)) in element.attributes.iter().enumerate() {
            bytes.push(if index + 1 == element.attributes.len() {
                6
            } else {
                0x46
            });
            name(key, bytes, base, names)?;
            substitution(value, bytes, values)?;
        }
        let length = (bytes.len() - attribute_size - 4) as u32;
        bytes[attribute_size..attribute_size + 4].copy_from_slice(&length.to_le_bytes());
    }
    bytes.push(2);
    if let Some(value) = &element.text {
        substitution(value, bytes, values)?;
    }
    for child in &element.children {
        encode_element(child, bytes, base, names, values)?;
    }
    bytes.push(4);
    let size = (bytes.len() - size_offset - 4) as u32;
    bytes[size_offset..size_offset + 4].copy_from_slice(&size.to_le_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_oversized_substitutions() {
        assert!(
            substitution(
                &Value::Text("x".repeat(65536)),
                &mut Vec::new(),
                &mut Vec::new()
            )
            .is_err()
        );
    }

    #[test]
    fn substitutions_preserve_native_scalar_types() {
        assert_eq!(
            Value::U16(1000).encoded(),
            (6, 1000u16.to_le_bytes().to_vec())
        );
        assert_eq!(
            Value::U32(u32::MAX).encoded(),
            (8, u32::MAX.to_le_bytes().to_vec())
        );
        assert_eq!(Value::Boolean(true).encoded(), (13, vec![1, 0, 0, 0]));
        assert_eq!(
            Value::Filetime(133_000_000_000_000_000).encoded(),
            (17, 133_000_000_000_000_000u64.to_le_bytes().to_vec())
        );
        assert_eq!(Value::I64(-1).encoded(), (9, vec![255; 8]));
    }
}
