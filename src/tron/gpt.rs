//! The GUID partition table at the start of the flash, read from bytes the caller has fetched.

use crate::mower::Part;
use anyhow::{Result, bail, ensure};

pub const SECTOR: usize = 512;
/// Header sector plus 32 sectors of entries: the room every GPT tool reserves for 128 entries of 128 bytes.
pub const PRIMARY_SECTORS: usize = 34;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub part: Part,
    /// The unique partition GUID as Linux prints it for PARTUUID.
    pub uuid: String,
}

/// CRC-32 as used by GPT (IEEE, reflected).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn guid_text(g: &[u8]) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{}",
        u32_at(g, 0),
        u16::from_le_bytes([g[4], g[5]]),
        u16::from_le_bytes([g[6], g[7]]),
        g[8],
        g[9],
        g[10..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

/// Parses the primary GPT from the first PRIMARY_SECTORS sectors of the flash. Both checksums must match.
pub fn parse_primary(start: &[u8]) -> Result<Vec<Entry>> {
    ensure!(
        start.len() >= PRIMARY_SECTORS * SECTOR,
        "too few sectors for a GPT"
    );
    let h = &start[SECTOR..2 * SECTOR];
    ensure!(&h[..8] == b"EFI PART", "no GPT header in sector 1");
    let header_len = u32_at(h, 12) as usize;
    ensure!(
        (92..=SECTOR).contains(&header_len),
        "GPT header size {header_len}"
    );
    let mut header = h[..header_len].to_vec();
    header[16..20].fill(0);
    ensure!(crc32(&header) == u32_at(h, 16), "GPT header checksum wrong");
    ensure!(
        u64_at(h, 24) == 1,
        "GPT header does not say it sits in sector 1"
    );

    let (entries_lba, count, size) = (
        u64_at(h, 72),
        u32_at(h, 80) as usize,
        u32_at(h, 84) as usize,
    );
    ensure!(
        entries_lba == 2 && size == 128 && count <= 128,
        "GPT entries at {entries_lba}, {count} x {size}"
    );
    let raw = &start[2 * SECTOR..2 * SECTOR + count * size];
    ensure!(crc32(raw) == u32_at(h, 88), "GPT entry checksum wrong");

    let mut out = Vec::new();
    for e in raw.chunks(size) {
        if e[..16].iter().all(|&b| b == 0) {
            continue;
        }
        let name: Vec<u16> = e[56..128]
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&c| c != 0)
            .collect();
        let (first, last) = (u64_at(e, 32), u64_at(e, 40));
        if last < first {
            bail!("GPT entry ends before it starts");
        }
        out.push(Entry {
            part: Part {
                name: String::from_utf16(&name)?,
                first_sector: first,
                last_sector: last,
            },
            uuid: guid_text(&e[16..32]),
        });
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A primary GPT as the vendor writes it: header in sector 1, entries from sector 2.
    pub fn image(parts: &[(&str, u64, u64, [u8; 16])]) -> Vec<u8> {
        let mut img = vec![0u8; PRIMARY_SECTORS * SECTOR];
        let mut entries = vec![0u8; 128 * 128];
        for (i, (name, first, last, uuid)) in parts.iter().enumerate() {
            let e = &mut entries[i * 128..(i + 1) * 128];
            e[..16].copy_from_slice(&[1; 16]);
            e[16..32].copy_from_slice(uuid);
            e[32..40].copy_from_slice(&first.to_le_bytes());
            e[40..48].copy_from_slice(&last.to_le_bytes());
            for (j, c) in name.encode_utf16().enumerate() {
                e[56 + 2 * j..58 + 2 * j].copy_from_slice(&c.to_le_bytes());
            }
        }
        let h = &mut img[SECTOR..SECTOR + 92];
        h[..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x10000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&1u64.to_le_bytes());
        h[72..80].copy_from_slice(&2u64.to_le_bytes());
        h[80..84].copy_from_slice(&128u32.to_le_bytes());
        h[84..88].copy_from_slice(&128u32.to_le_bytes());
        h[88..92].copy_from_slice(&crc32(&entries).to_le_bytes());
        let crc = crc32(h);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
        img[2 * SECTOR..2 * SECTOR + entries.len()].copy_from_slice(&entries);
        img
    }

    pub const ROOTFS_UUID: [u8; 16] = [0, 0, 0x4e, 0x61, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn reads_names_bounds_and_partuuid() {
        let img = image(&[
            ("boot", 32768, 557055, [7; 16]),
            ("rootfs", 13664256, 47218687, ROOTFS_UUID),
        ]);
        let t = parse_primary(&img).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(
            t[1].part,
            Part {
                name: "rootfs".into(),
                first_sector: 13664256,
                last_sector: 47218687
            }
        );
        assert_eq!(t[1].uuid, "614e0000-0000-0000-0000-000000000000");
    }

    #[test]
    fn a_changed_byte_is_refused() {
        let mut img = image(&[("rootfs", 100, 200, ROOTFS_UUID)]);
        img[2 * SECTOR + 40] ^= 1;
        assert!(
            parse_primary(&img)
                .unwrap_err()
                .to_string()
                .contains("entry checksum")
        );
        let mut img = image(&[("rootfs", 100, 200, ROOTFS_UUID)]);
        img[SECTOR + 80] ^= 1;
        assert!(
            parse_primary(&img)
                .unwrap_err()
                .to_string()
                .contains("header checksum")
        );
        assert!(parse_primary(&vec![0u8; PRIMARY_SECTORS * SECTOR]).is_err());
    }
}
