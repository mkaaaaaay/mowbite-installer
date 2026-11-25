//! Backup files in the seekable zstd format: independent frames of FRAME raw bytes each, then a skippable frame
//! with the seek table. Plain zstd still unpacks such a file front to back; ext4 reads take single frames.

use anyhow::{Context, Result, bail, ensure};
use ext4_view::Ext4Read;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

/// Raw bytes per frame.
pub const FRAME: usize = 4 << 20;
const LEVEL: i32 = 3;
const SKIPPABLE_MAGIC: u32 = 0x184D_2A5E;
const SEEKABLE_MAGIC: u32 = 0x8F92_EAB1;
const FOOTER: u64 = 9;
/// Unpacked frames kept for reads close to each other.
const CACHED: usize = 8;

/// Packs everything written into frames, several frames side by side on all cores; finish() writes the seek table.
pub struct FrameWriter<W: Write> {
    out: W,
    filling: Vec<u8>,
    full: Vec<Vec<u8>>,
    batch: usize,
    sizes: Vec<(u32, u32)>,
}

impl<W: Write> FrameWriter<W> {
    pub fn new(out: W) -> Self {
        FrameWriter {
            out,
            filling: Vec::with_capacity(FRAME),
            full: Vec::new(),
            batch: std::thread::available_parallelism().map_or(1, |n| n.get()),
            sizes: Vec::new(),
        }
    }

    pub fn write_all(&mut self, mut data: &[u8]) -> Result<()> {
        while !data.is_empty() {
            let n = (FRAME - self.filling.len()).min(data.len());
            self.filling.extend_from_slice(&data[..n]);
            data = &data[n..];
            if self.filling.len() == FRAME {
                self.full.push(std::mem::replace(
                    &mut self.filling,
                    Vec::with_capacity(FRAME),
                ));
                if self.full.len() == self.batch {
                    self.pack_full()?;
                }
            }
        }
        Ok(())
    }

    fn pack_full(&mut self) -> Result<()> {
        let raw = std::mem::take(&mut self.full);
        let packed: Vec<Result<Vec<u8>>> = std::thread::scope(|s| {
            let jobs: Vec<_> = raw.iter().map(|r| s.spawn(|| pack(r))).collect();
            jobs.into_iter()
                .map(|j| j.join().unwrap_or_else(|_| bail!("packing a frame failed")))
                .collect()
        });
        for (r, p) in raw.iter().zip(packed) {
            let p = p?;
            self.out.write_all(&p)?;
            self.sizes
                .push((u32::try_from(p.len())?, u32::try_from(r.len())?));
        }
        Ok(())
    }

    /// Packs what is left and appends the seek table; returns the inner writer.
    pub fn finish(mut self) -> Result<W> {
        if !self.filling.is_empty() {
            self.full.push(std::mem::take(&mut self.filling));
        }
        self.pack_full()?;
        let n = u32::try_from(self.sizes.len())?;
        let mut table = Vec::with_capacity(8 + self.sizes.len() * 8 + FOOTER as usize);
        table.extend_from_slice(&SKIPPABLE_MAGIC.to_le_bytes());
        table.extend_from_slice(&(n * 8 + FOOTER as u32).to_le_bytes());
        for (c, d) in &self.sizes {
            table.extend_from_slice(&c.to_le_bytes());
            table.extend_from_slice(&d.to_le_bytes());
        }
        table.extend_from_slice(&n.to_le_bytes());
        table.push(0);
        table.extend_from_slice(&SEEKABLE_MAGIC.to_le_bytes());
        self.out.write_all(&table)?;
        Ok(self.out)
    }
}

/// One independent frame with its content size and checksum, so each frame proves itself when unpacked.
fn pack(raw: &[u8]) -> Result<Vec<u8>> {
    let mut c = zstd::bulk::Compressor::new(LEVEL)?;
    c.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true))?;
    Ok(c.compress(raw)?)
}

struct Frame {
    packed_at: u64,
    packed_len: u32,
    raw_at: u64,
    raw_len: u32,
}

/// Random reads of the raw bytes of a seekable file.
pub struct SeekableReader {
    file: File,
    frames: Vec<Frame>,
    len: u64,
    cache: Vec<(usize, Vec<u8>)>,
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

impl SeekableReader {
    pub fn open(mut file: File) -> Result<Self> {
        let size = file.metadata()?.len();
        ensure!(size >= 8 + FOOTER, "too short for a seek table");
        let mut footer = [0u8; FOOTER as usize];
        file.seek(SeekFrom::Start(size - FOOTER))?;
        file.read_exact(&mut footer)?;
        ensure!(
            u32_at(&footer, 5) == SEEKABLE_MAGIC && footer[4] == 0,
            "no seek table of this installer"
        );
        let n = u64::from(u32_at(&footer, 0));
        let table_len = n * 8 + FOOTER;
        let start = size
            .checked_sub(8 + table_len)
            .context("seek table longer than the file")?;
        let mut table = vec![0u8; (8 + table_len) as usize];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut table)?;
        ensure!(
            u32_at(&table, 0) == SKIPPABLE_MAGIC && u64::from(u32_at(&table, 4)) == table_len,
            "broken seek table"
        );
        let mut frames = Vec::with_capacity(n as usize);
        let (mut packed_at, mut raw_at) = (0u64, 0u64);
        for i in 0..n as usize {
            let packed_len = u32_at(&table, 8 + i * 8);
            let raw_len = u32_at(&table, 12 + i * 8);
            frames.push(Frame {
                packed_at,
                packed_len,
                raw_at,
                raw_len,
            });
            packed_at += u64::from(packed_len);
            raw_at += u64::from(raw_len);
        }
        ensure!(
            packed_at == start,
            "the seek table does not match the frames"
        );
        Ok(SeekableReader {
            file,
            frames,
            len: raw_at,
            cache: Vec::new(),
        })
    }

    /// Raw length of the whole file.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> u64 {
        self.len
    }

    fn frame(&mut self, i: usize) -> Result<&[u8]> {
        if let Some(at) = self.cache.iter().position(|(f, _)| *f == i) {
            let hit = self.cache.remove(at);
            self.cache.push(hit);
        } else {
            let f = &self.frames[i];
            let mut packed = vec![0u8; f.packed_len as usize];
            self.file.seek(SeekFrom::Start(f.packed_at))?;
            self.file.read_exact(&mut packed)?;
            let raw = zstd::bulk::Decompressor::new()?.decompress(&packed, f.raw_len as usize)?;
            ensure!(
                raw.len() == f.raw_len as usize,
                "frame {i} is shorter than the table says"
            );
            if self.cache.len() == CACHED {
                self.cache.remove(0);
            }
            self.cache.push((i, raw));
        }
        Ok(&self.cache.last().expect("just pushed").1)
    }

    pub fn read_at(&mut self, mut at: u64, mut dst: &mut [u8]) -> Result<()> {
        ensure!(
            at.checked_add(dst.len() as u64)
                .is_some_and(|end| end <= self.len),
            "read past the end"
        );
        while !dst.is_empty() {
            let i = self
                .frames
                .partition_point(|f| f.raw_at + u64::from(f.raw_len) <= at);
            let off = (at - self.frames[i].raw_at) as usize;
            let raw = self.frame(i)?;
            let n = (raw.len() - off).min(dst.len());
            dst[..n].copy_from_slice(&raw[off..off + n]);
            dst = &mut dst[n..];
            at += n as u64;
        }
        Ok(())
    }
}

impl Ext4Read for SeekableReader {
    fn read(
        &mut self,
        start: u64,
        dst: &mut [u8],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.read_at(start, dst)
            .map_err(|e| format!("{e:#}").into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 / 4096 % 251) as u8).collect()
    }

    fn packed(data: &[u8]) -> tempfile::NamedTempFile {
        let f = tempfile::NamedTempFile::new().unwrap();
        let mut w = FrameWriter::new(f.reopen().unwrap());
        // Uneven pieces, as read calls never line up with frames.
        for piece in data.chunks(1_000_003) {
            w.write_all(piece).unwrap();
        }
        w.finish().unwrap();
        f
    }

    #[test]
    fn plain_zstd_unpacks_it_and_random_reads_match() {
        let data = sample(3 * FRAME + 12345);
        let f = packed(&data);
        let mut plain = Vec::new();
        zstd::Decoder::new(f.reopen().unwrap())
            .unwrap()
            .read_to_end(&mut plain)
            .unwrap();
        assert_eq!(plain, data);
        let mut r = SeekableReader::open(f.reopen().unwrap()).unwrap();
        assert_eq!(r.len(), data.len() as u64);
        assert_eq!(r.frames.len(), 4);
        for (at, len) in [
            (0, 10),
            (FRAME - 5, 10),
            (2 * FRAME - 1, FRAME + 2),
            (data.len() - 7, 7),
            (5, 0),
        ] {
            let mut got = vec![0u8; len];
            r.read_at(at as u64, &mut got).unwrap();
            assert_eq!(got, data[at..at + len], "at {at}");
        }
        assert!(r.read_at(data.len() as u64 - 3, &mut [0u8; 4]).is_err());
    }

    #[test]
    fn an_empty_area_has_an_empty_table() {
        let f = packed(&[]);
        let r = SeekableReader::open(f.reopen().unwrap()).unwrap();
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn a_single_frame_file_or_a_changed_frame_is_refused() {
        let data = sample(2 * FRAME);
        let mut old = Vec::new();
        zstd::stream::copy_encode(&data[..], &mut old, 3).unwrap();
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), &old).unwrap();
        assert!(SeekableReader::open(f.reopen().unwrap()).is_err());

        let f = packed(&data);
        let mut bytes = std::fs::read(f.path()).unwrap();
        bytes[100] ^= 1;
        std::fs::write(f.path(), &bytes).unwrap();
        let mut r = SeekableReader::open(f.reopen().unwrap()).unwrap();
        assert!(r.read_at(0, &mut [0u8; 16]).is_err());
        let mut later = [0u8; 16];
        r.read_at(FRAME as u64, &mut later).unwrap();
        assert_eq!(later, data[FRAME..FRAME + 16]);
    }
}
