//! Stable primary event category precedence for combined USN reasons.

#[must_use]
pub fn category(reason: u32) -> (u32, &'static str) {
    for (mask, id, name) in [
        (0x200, 1001, "File/directory deleted"),
        (0x2000, 1003, "Rename - new name"),
        (0x1000, 1002, "Rename - old name"),
        (0x100, 1000, "File/directory created"),
        (0x800, 1020, "Security changed"),
        (0x00400077, 1010, "Data changed"),
        (0x00bfc400, 1030, "Attributes/metadata changed"),
    ] {
        if reason & mask != 0 {
            return (id, name);
        }
    }
    (1099, "Other journal change")
}
