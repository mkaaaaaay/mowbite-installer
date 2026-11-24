//! The tron section of the signed tables. The frame verifies the signature and hands over only this section.

use super::checks::Known;
use crate::mower::Part;
use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// The section as shipped with this build; the release signs the same content.
pub const SHIPPED: &[u8] = include_bytes!("../../assets/tron/tables.json");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Section {
    partition_tables: Vec<Vec<Part>>,
    required: Vec<String>,
    optional: Vec<String>,
    vendor_versions: Vec<String>,
}

pub fn parse(raw: &[u8]) -> Result<Known> {
    let s: Section = serde_json::from_slice(raw).context("tron tables")?;
    if s.partition_tables.is_empty() {
        bail!("tron tables: no partition table");
    }
    for t in &s.partition_tables {
        if t.iter().filter(|p| p.name == "rootfs").count() != 1 {
            bail!("tron tables: a partition table without exactly one rootfs");
        }
        if t.iter().any(|p| p.first_sector > p.last_sector)
            || t.windows(2).any(|w| w[0].last_sector >= w[1].first_sector)
        {
            bail!("tron tables: partitions out of order or overlapping");
        }
    }
    if s.required.is_empty() {
        bail!("tron tables: no required parts");
    }
    for p in s.required.iter().chain(&s.optional) {
        if !p.starts_with('/')
            || p.ends_with('/')
            || p.split('/').skip(1).any(|c| matches!(c, "" | "." | ".."))
        {
            bail!("tron tables: part {p:?} is not a plain absolute path");
        }
    }
    Ok(Known {
        tables: s.partition_tables,
        required: s.required,
        optional: s.optional,
        vendor_versions: s.vendor_versions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_section_parses() {
        let k = parse(SHIPPED).unwrap();
        assert_eq!(k.tables[0].len(), 8);
        assert!(k.required.contains(&"/vendor/etc/firmware".to_string()));
    }

    #[test]
    fn bad_sections_are_refused() {
        let table = r#"[[{"name":"rootfs","first_sector":10,"last_sector":20}]]"#;
        let good = format!(
            r#"{{"partition_tables":{table},"required":["/a/b"],"optional":[],"vendor_versions":[]}}"#
        );
        assert!(parse(good.as_bytes()).is_ok());
        for bad in [
            r#"{"partition_tables":[],"required":["/a"],"optional":[],"vendor_versions":[]}"#.to_string(),
            format!(r#"{{"partition_tables":{table},"required":[],"optional":[],"vendor_versions":[]}}"#),
            format!(r#"{{"partition_tables":{table},"required":["/a/../etc"],"optional":[],"vendor_versions":[]}}"#),
            format!(r#"{{"partition_tables":{table},"required":["a"],"optional":[],"vendor_versions":[]}}"#),
            format!(r#"{{"partition_tables":{table},"required":["/a"],"optional":["/b//c"],"vendor_versions":[]}}"#),
            format!(r#"{{"partition_tables":{table},"required":["/a"],"optional":[],"vendor_versions":[],"x":1}}"#),
            r#"{"partition_tables":[[{"name":"boot","first_sector":1,"last_sector":2}]],"required":["/a"],"optional":[],"vendor_versions":[]}"#.to_string(),
            r#"{"partition_tables":[[{"name":"rootfs","first_sector":1,"last_sector":9},{"name":"oem","first_sector":9,"last_sector":12}]],"required":["/a"],"optional":[],"vendor_versions":[]}"#.to_string(),
        ] {
            assert!(parse(bad.as_bytes()).is_err(), "{bad}");
        }
    }
}
