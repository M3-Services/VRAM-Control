//! Parsing of Windows performance counter instance names.
//!
//! Process memory instances look like `pid_1916_luid_0x00000000_0x0001232b_phys_0`,
//! adapter memory instances like `luid_0x00000000_0x0001232b_phys_0`.

use crate::model::Luid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessInstance {
    pub pid: u32,
    pub luid: Luid,
    pub phys: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterInstance {
    pub luid: Luid,
    pub phys: u32,
}

fn parse_hex(part: &str) -> Option<u32> {
    u32::from_str_radix(part.strip_prefix("0x")?, 16).ok()
}

/// Parses `luid_<hex>_<hex>_phys_<n>` parts starting at `parts[0]`.
fn parse_luid_phys(parts: &[&str]) -> Option<(Luid, u32)> {
    if parts.len() < 5 || parts[0] != "luid" || parts[3] != "phys" {
        return None;
    }
    let luid = Luid::new(parse_hex(parts[1])?, parse_hex(parts[2])?);
    let phys = parts[4].parse().ok()?;
    Some((luid, phys))
}

pub fn parse_process_instance(name: &str) -> Option<ProcessInstance> {
    let parts: Vec<&str> = name.split('_').collect();
    if parts.len() < 7 || parts[0] != "pid" {
        return None;
    }
    let pid = parts[1].parse().ok()?;
    let (luid, phys) = parse_luid_phys(&parts[2..])?;
    Some(ProcessInstance { pid, luid, phys })
}

pub fn parse_adapter_instance(name: &str) -> Option<AdapterInstance> {
    let parts: Vec<&str> = name.split('_').collect();
    let (luid, phys) = parse_luid_phys(&parts)?;
    Some(AdapterInstance { luid, phys })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_process_instance() {
        let got = parse_process_instance("pid_1916_luid_0x00000000_0x0001232b_phys_0");
        assert_eq!(
            got,
            Some(ProcessInstance {
                pid: 1916,
                luid: Luid::new(0, 0x0001232b),
                phys: 0
            })
        );
    }

    #[test]
    fn parses_a_process_instance_with_a_large_pid_and_phys() {
        let got = parse_process_instance("pid_45636_luid_0x00000001_0xdeadbeef_phys_2").unwrap();
        assert_eq!(got.pid, 45636);
        assert_eq!(got.luid, Luid::new(1, 0xdeadbeef));
        assert_eq!(got.phys, 2);
    }

    #[test]
    fn rejects_malformed_process_instances() {
        assert_eq!(parse_process_instance(""), None);
        assert_eq!(parse_process_instance("pid_abc_luid_0x0_0x1_phys_0"), None);
        assert_eq!(parse_process_instance("pid_1_luid_0x0_0x1"), None);
        assert_eq!(parse_process_instance("foo_1_luid_0x0_0x1_phys_0"), None);
        assert_eq!(parse_process_instance("pid_1_luid_0xZZ_0x1_phys_0"), None);
        assert_eq!(parse_process_instance("_Total"), None);
    }

    #[test]
    fn parses_an_adapter_instance() {
        let got = parse_adapter_instance("luid_0x00000000_0x0001232b_phys_0");
        assert_eq!(
            got,
            Some(AdapterInstance {
                luid: Luid::new(0, 0x0001232b),
                phys: 0
            })
        );
    }

    #[test]
    fn rejects_malformed_adapter_instances() {
        assert_eq!(parse_adapter_instance(""), None);
        assert_eq!(parse_adapter_instance("luid_0x0_0x1"), None);
        assert_eq!(parse_adapter_instance("pid_1_luid_0x0_0x1_phys_0"), None);
    }
}
