//! Synthetic USN buffers for tests; never accesses a host journal.

#[must_use]
pub fn batch(major: u16, usn: i64, reason: u32, name: &[u16]) -> Vec<u8> {
    let fixed = if major == 3 { 76 } else { 60 };
    let u = if major == 3 { 40 } else { 24 };
    let len = (fixed + name.len() * 2 + 7) & !7;
    let mut bytes = vec![0; 8 + len];
    bytes[..8].copy_from_slice(&(usn + len as i64).to_le_bytes());
    let r = &mut bytes[8..];
    r[..4].copy_from_slice(&(len as u32).to_le_bytes());
    r[4..6].copy_from_slice(&major.to_le_bytes());
    r[8] = 0xab;
    r[u..u + 8].copy_from_slice(&usn.to_le_bytes());
    r[u + 8..u + 16].copy_from_slice(&133_000_000_000_000_000_i64.to_le_bytes());
    r[u + 16..u + 20].copy_from_slice(&reason.to_le_bytes());
    r[fixed - 4..fixed - 2].copy_from_slice(&((name.len() * 2) as u16).to_le_bytes());
    r[fixed - 2..fixed].copy_from_slice(&(fixed as u16).to_le_bytes());
    for (i, c) in name.iter().enumerate() {
        r[fixed + i * 2..fixed + i * 2 + 2].copy_from_slice(&c.to_le_bytes());
    }
    bytes
}
