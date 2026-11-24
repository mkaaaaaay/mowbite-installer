//! The FIT image on the boot partition: which kernel starts, with which command line, and whether it is signed.
//! The input comes from the owner's device, so every offset is checked before use.

use anyhow::{Context, Result, bail, ensure};
use std::ops::Range;

const FDT_MAGIC: u32 = 0xd00d_feed;

/// A flattened device tree as a flat list of properties: (node path, property name, byte range in the blob).
pub struct Fdt<'a> {
    blob: &'a [u8],
    props: Vec<(String, String, Range<usize>)>,
}

fn be32(b: &[u8], o: usize) -> Result<u32> {
    let end = o.checked_add(4).context("device tree offset too large")?;
    let s = b.get(o..end).context("device tree cut short")?;
    Ok(u32::from_be_bytes(s.try_into().unwrap()))
}

/// `at + len` rounded up to the next multiple of 4.
fn align4(at: usize, len: usize) -> Result<usize> {
    Ok(at
        .checked_add(len)
        .and_then(|n| n.checked_add(3))
        .context("device tree offset too large")?
        & !3)
}

fn c_str(b: &[u8], o: usize) -> Result<&str> {
    let rest = b.get(o..).context("device tree string out of range")?;
    let end = rest
        .iter()
        .position(|&c| c == 0)
        .context("device tree string without end")?;
    Ok(std::str::from_utf8(&rest[..end])?)
}

impl<'a> Fdt<'a> {
    pub fn parse(blob: &'a [u8]) -> Result<Self> {
        ensure!(be32(blob, 0)? == FDT_MAGIC, "no device tree");
        let total = be32(blob, 4)? as usize;
        ensure!(total <= blob.len(), "device tree longer than its data");
        let blob = &blob[..total];
        let (structs, strings) = (be32(blob, 8)? as usize, be32(blob, 12)? as usize);
        let mut props = Vec::new();
        let mut path: Vec<&str> = Vec::new();
        let mut p = structs;
        loop {
            let token = be32(blob, p)?;
            p += 4;
            match token {
                1 => {
                    let name = c_str(blob, p)?;
                    p = align4(p, name.len() + 1)?;
                    path.push(name);
                }
                2 => {
                    path.pop()
                        .context("device tree closes a node it never opened")?;
                }
                3 => {
                    let (len, name_off) = (be32(blob, p)? as usize, be32(blob, p + 4)? as usize);
                    p += 8;
                    let range = p..p.checked_add(len).context("property too long")?;
                    ensure!(
                        range.end <= blob.len(),
                        "property past the end of the device tree"
                    );
                    let at = strings
                        .checked_add(name_off)
                        .context("device tree string offset too large")?;
                    let name = c_str(blob, at)?;
                    ensure!(!path.is_empty(), "device tree property outside a node");
                    props.push((path[1..].join("/"), name.to_string(), range));
                    p = align4(p, len)?;
                }
                4 => {}
                9 => break,
                t => bail!("unknown device tree token {t}"),
            }
        }
        Ok(Fdt { blob, props })
    }

    /// Node paths without the leading slash, "" for the root.
    pub fn get(&self, node: &str, prop: &str) -> Option<&'a [u8]> {
        self.props
            .iter()
            .find(|(n, p, _)| n == node && p == prop)
            .map(|(_, _, r)| &self.blob[r.clone()])
    }

    pub fn text(&self, node: &str, prop: &str) -> Option<String> {
        let v = self.get(node, prop)?;
        let v = v.strip_suffix(&[0]).unwrap_or(v);
        Some(String::from_utf8_lossy(v).into_owned())
    }

    fn u32(&self, node: &str, prop: &str) -> Option<usize> {
        let v = self.get(node, prop)?;
        Some(u32::from_be_bytes(v.try_into().ok()?) as usize)
    }

    pub fn nodes(&self) -> impl Iterator<Item = &str> {
        self.props.iter().map(|(n, _, _)| n.as_str())
    }
}

/// What the boot partition starts.
#[derive(Debug, PartialEq, Eq)]
pub struct Boot {
    /// "5.10.209": the name of the module folder the kernel loads from.
    pub kernel_release: String,
    pub kernel_banner: String,
    /// /chosen/bootargs of the device tree in the default configuration, empty when there is none.
    pub bootargs: String,
    /// A signature node with a value anywhere in the FIT: the loader may refuse an image it did not sign.
    pub signed: bool,
}

fn image_data<'a>(part: &'a [u8], fit: &Fdt<'a>, node: &str) -> Result<&'a [u8]> {
    if let Some(d) = fit.get(node, "data") {
        return Ok(d);
    }
    let size = fit
        .u32(node, "data-size")
        .with_context(|| format!("{node} has no data-size"))?;
    let start = if let Some(pos) = fit.u32(node, "data-position") {
        pos
    } else {
        let off = fit
            .u32(node, "data-offset")
            .with_context(|| format!("{node} has no data"))?;
        // External data starts right after the FIT header, 4-byte aligned.
        align4(be32(part, 4)? as usize, 0)?
            .checked_add(off)
            .context("data offset too large")?
    };
    let end = start.checked_add(size).context("data size too large")?;
    part.get(start..end)
        .with_context(|| format!("{node} data past the end of the partition"))
}

pub fn read_boot(part: &[u8]) -> Result<Boot> {
    let fit = Fdt::parse(part).context("boot partition")?;
    let conf = fit
        .text("configurations", "default")
        .context("FIT without default configuration")?;
    let conf = format!("configurations/{conf}");
    let kernel = fit
        .text(&conf, "kernel")
        .context("FIT configuration without kernel")?;
    let kernel = format!("images/{kernel}");
    let compression = fit
        .text(&kernel, "compression")
        .unwrap_or_else(|| "none".into());
    ensure!(
        compression == "none",
        "kernel is {compression} compressed, its version cannot be read"
    );
    let image = image_data(part, &fit, &kernel)?;

    const MARK: &[u8] = b"Linux version ";
    let at = image
        .windows(MARK.len())
        .position(|w| w == MARK)
        .context("no Linux version line in the kernel")?;
    let banner: String = image[at..]
        .iter()
        .take_while(|&&c| (0x20..0x7f).contains(&c))
        .map(|&c| c as char)
        .collect();
    let kernel_release = banner[MARK.len()..]
        .split(' ')
        .next()
        .unwrap_or_default()
        .to_string();
    ensure!(!kernel_release.is_empty(), "empty kernel version");

    let bootargs = match fit.text(&conf, "fdt") {
        Some(fdt) => Fdt::parse(image_data(part, &fit, &format!("images/{fdt}"))?)
            .context("device tree in the FIT")?
            .text("chosen", "bootargs")
            .unwrap_or_default(),
        None => String::new(),
    };

    let signed = fit.nodes().any(|n| {
        n.rsplit('/')
            .next()
            .is_some_and(|last| last.starts_with("signature"))
            && fit.get(n, "value").is_some()
    });
    Ok(Boot {
        kernel_release,
        kernel_banner: banner,
        bootargs,
        signed,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a device tree from (node path, property, value); nodes appear in the order first named.
    pub fn fdt(props: &[(&str, &str, &[u8])]) -> Vec<u8> {
        let mut strings = Vec::new();
        let mut structs = Vec::new();
        let mut open: Vec<String> = Vec::new();
        let put = |s: &mut Vec<u8>, v: u32| s.extend_from_slice(&v.to_be_bytes());
        let pad = |s: &mut Vec<u8>| s.resize((s.len() + 3) & !3, 0);
        let mut order: Vec<&str> = vec![""];
        for (n, _, _) in props {
            if !order.contains(n) {
                order.push(n);
            }
        }
        for node in order {
            let parts: Vec<String> = if node.is_empty() {
                vec![String::new()]
            } else {
                std::iter::once(String::new())
                    .chain(node.split('/').map(String::from))
                    .collect()
            };
            let common = open.iter().zip(&parts).take_while(|(a, b)| a == b).count();
            for _ in common..open.len() {
                put(&mut structs, 2);
            }
            open.truncate(common);
            for name in &parts[common..] {
                put(&mut structs, 1);
                structs.extend_from_slice(name.as_bytes());
                structs.push(0);
                pad(&mut structs);
                open.push(name.clone());
            }
            for (_, p, v) in props.iter().filter(|(n, _, _)| *n == node) {
                put(&mut structs, 3);
                put(&mut structs, v.len() as u32);
                put(&mut structs, strings.len() as u32);
                strings.extend_from_slice(p.as_bytes());
                strings.push(0);
                structs.extend_from_slice(v);
                pad(&mut structs);
            }
        }
        for _ in &open {
            put(&mut structs, 2);
        }
        put(&mut structs, 9);
        let off_struct = 40;
        let off_strings = off_struct + structs.len();
        let total = off_strings + strings.len();
        let mut out = Vec::new();
        for v in [
            FDT_MAGIC,
            total as u32,
            off_struct as u32,
            off_strings as u32,
            40,
            17,
            16,
            0,
            strings.len() as u32,
            structs.len() as u32,
        ] {
            put(&mut out, v);
        }
        out.extend_from_slice(&structs);
        out.extend_from_slice(&strings);
        out
    }

    /// A boot partition like the vendor's: kernel and device tree outside the FIT header, at data-position.
    pub fn boot_partition(kernel: &[u8], bootargs: &str, signature_value: bool) -> Vec<u8> {
        let mut args = bootargs.as_bytes().to_vec();
        args.push(0);
        let dtb = fdt(&[("chosen", "bootargs", &args)]);
        let (k_pos, d_pos) = (4096u32, 4096 + kernel.len() as u32);
        let mut props: Vec<(&str, &str, &[u8])> = vec![
            ("configurations", "default", b"conf\0"),
            ("configurations/conf", "kernel", b"kernel\0"),
            ("configurations/conf", "fdt", b"fdt\0"),
            ("configurations/conf/signature", "algo", b"sha256,rsa2048\0"),
        ];
        let k_size = (kernel.len() as u32).to_be_bytes();
        let d_size = (dtb.len() as u32).to_be_bytes();
        let (kp, dp) = (k_pos.to_be_bytes(), d_pos.to_be_bytes());
        if signature_value {
            props.push(("configurations/conf/signature", "value", &[1, 2, 3, 4]));
        }
        props.extend_from_slice(&[
            ("images/kernel", "data-size", &k_size[..]),
            ("images/kernel", "data-position", &kp[..]),
            ("images/kernel", "compression", b"none\0"),
            ("images/fdt", "data-size", &d_size[..]),
            ("images/fdt", "data-position", &dp[..]),
        ]);
        let mut part = fdt(&props);
        part.resize(k_pos as usize, 0);
        part.extend_from_slice(kernel);
        part.extend_from_slice(&dtb);
        part.resize(part.len() + 4096, 0);
        part
    }

    pub const KERNEL: &[u8] = b"\x00\x01Linux version 5.10.209 (nie@ubuntu) (gcc) #2 SMP\n\x00rest";

    #[test]
    fn reads_kernel_version_and_command_line() {
        let part = boot_partition(
            KERNEL,
            "console=ttyFIQ0 root=PARTUUID=614e0000-0000 rw rootwait",
            false,
        );
        let b = read_boot(&part).unwrap();
        assert_eq!(b.kernel_release, "5.10.209");
        assert_eq!(
            b.kernel_banner,
            "Linux version 5.10.209 (nie@ubuntu) (gcc) #2 SMP"
        );
        assert_eq!(
            b.bootargs,
            "console=ttyFIQ0 root=PARTUUID=614e0000-0000 rw rootwait"
        );
        assert!(!b.signed);
    }

    #[test]
    fn a_signature_value_marks_the_image_signed() {
        assert!(
            read_boot(&boot_partition(KERNEL, "rw", true))
                .unwrap()
                .signed
        );
    }

    #[test]
    fn broken_input_is_an_error_not_a_panic() {
        let part = boot_partition(KERNEL, "rw", false);
        for cut in [0, 3, 40, 200, 4100] {
            let _ = read_boot(&part[..cut]);
        }
        let mut bad = part.clone();
        bad[8..12].copy_from_slice(&0xffff_fff0u32.to_be_bytes());
        assert!(read_boot(&bad).is_err());
        assert!(read_boot(&boot_partition(b"no banner here", "rw", false)).is_err());
    }
}
