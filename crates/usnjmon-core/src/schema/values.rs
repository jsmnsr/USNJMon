use super::Data;
use anyhow::{Context, Result};
use serde_json::Value;

pub(super) fn text(value: &Value, key: &str) -> Result<Data> {
    Ok(Data::Text(
        value
            .get(key)
            .and_then(Value::as_str)
            .with_context(|| format!("missing/invalid text field {key}"))?
            .into(),
    ))
}
pub(super) fn u64_field(value: &Value, key: &str) -> Result<u64> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .with_context(|| format!("missing/invalid unsigned field {key}"))
}
pub(super) fn u32_field(value: &Value, key: &str) -> Result<Data> {
    Ok(Data::U32(u64_field(value, key)?.try_into()?))
}
pub(super) fn u16_field(value: &Value, key: &str) -> Result<Data> {
    Ok(Data::U16(u64_field(value, key)?.try_into()?))
}
pub(super) fn i64_field(value: &Value, key: &str) -> Result<Data> {
    Ok(Data::I64(
        value
            .get(key)
            .and_then(Value::as_i64)
            .with_context(|| format!("missing/invalid signed field {key}"))?,
    ))
}
pub(super) fn bool_field(value: &Value, key: &str) -> Result<Data> {
    Ok(Data::Boolean(
        value
            .get(key)
            .and_then(Value::as_bool)
            .with_context(|| format!("missing/invalid boolean field {key}"))?,
    ))
}

pub(super) fn checkpoint_fields(value: &Value) -> Result<(Data, Data, Data)> {
    if value.is_null() {
        Ok((Data::Text(String::new()), Data::U64(0), Data::I64(0)))
    } else {
        Ok((
            text(value, "volume_identity")?,
            Data::U64(u64_field(value, "journal_id")?),
            i64_field(value, "next_usn")?,
        ))
    }
}
