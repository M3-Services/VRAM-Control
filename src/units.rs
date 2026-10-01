//! Human-readable formatting of byte counts.

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;

/// Formats a byte count using binary units: `0 B`, `512 KiB`, `771 MiB`, `15.4 GiB`.
pub fn format_bytes(bytes: u64) -> String {
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{} MiB", bytes / MIB)
    } else if bytes >= KIB {
        format!("{} KiB", bytes / KIB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_each_unit() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1 KiB");
        assert_eq!(format_bytes(512 * 1024), "512 KiB");
        assert_eq!(format_bytes(771 * 1024 * 1024), "771 MiB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GiB");
        assert_eq!(format_bytes(15_800 * 1024 * 1024), "15.4 GiB");
    }
}
