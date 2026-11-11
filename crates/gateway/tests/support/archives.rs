//! Small archive writers for the toolbox tests: a `.tar.gz` and a `.zip`
//! holding what the test says, bad entries included (a `..` path, a link),
//! so the unpacker is tried on archives no honest tool would write.

#![allow(dead_code)]

use std::io::Write;

/// One entry of a test archive.
pub struct Entry<'a> {
    pub name: &'a str,
    pub data: &'a [u8],
    /// `Some(target)`: a symbolic link instead of a file.
    pub link: Option<&'a str>,
    /// Unix permissions (`0o755` for a program).
    pub mode: u32,
}

impl<'a> Entry<'a> {
    pub fn file(name: &'a str, data: &'a [u8]) -> Self {
        Entry {
            name,
            data,
            link: None,
            mode: 0o644,
        }
    }
    pub fn program(name: &'a str, data: &'a [u8]) -> Self {
        Entry {
            name,
            data,
            link: None,
            mode: 0o755,
        }
    }
    pub fn link(name: &'a str, target: &'a str) -> Self {
        Entry {
            name,
            data: &[],
            link: Some(target),
            mode: 0o777,
        }
    }
}

fn octal(buf: &mut [u8], value: u64) {
    let s = format!("{value:0width$o}", width = buf.len() - 1);
    buf[..s.len()].copy_from_slice(s.as_bytes());
}

/// A tar header as GNU tar writes it (the format of the real GeoLibre
/// archives: magic `ustar  \0`).
fn tar_header(name: &str, size: u64, typeflag: u8, link: &str, mode: u32) -> [u8; 512] {
    let mut h = [0u8; 512];
    let n = name.as_bytes();
    assert!(n.len() <= 100, "test names fit the header");
    h[..n.len()].copy_from_slice(n);
    octal(&mut h[100..108], mode as u64);
    octal(&mut h[108..116], 1001);
    octal(&mut h[116..124], 1001);
    octal(&mut h[124..136], size);
    octal(&mut h[136..148], 1_762_800_000);
    h[156] = typeflag;
    let l = link.as_bytes();
    h[157..157 + l.len()].copy_from_slice(l);
    h[257..265].copy_from_slice(b"ustar  \0");
    h[265..271].copy_from_slice(b"runner");
    // The checksum is taken with its own field as spaces.
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|b| *b as u32).sum();
    let s = format!("{sum:06o}\0 ");
    h[148..156].copy_from_slice(s.as_bytes());
    h
}

/// A `.tar.gz` of `entries`, each a regular file or a link. A name ending
/// in `/` is a folder.
pub fn tar_gz(entries: &[Entry]) -> Vec<u8> {
    let mut tar = Vec::new();
    for e in entries {
        let (flag, size, link) = match e.link {
            Some(target) => (b'2', 0, target),
            None if e.name.ends_with('/') => (b'5', 0, ""),
            None => (b'0', e.data.len() as u64, ""),
        };
        tar.extend_from_slice(&tar_header(e.name, size, flag, link, e.mode));
        if flag == b'0' {
            tar.extend_from_slice(e.data);
            tar.resize(tar.len().div_ceil(512) * 512, 0);
        }
    }
    tar.extend_from_slice(&[0u8; 1024]);
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&tar).unwrap();
    gz.finish().unwrap()
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(data);
    c.sum()
}

/// A `.zip` of `entries` (deflated; links as Unix symbolic-link entries).
pub fn zip(entries: &[Entry]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for e in entries {
        let raw: Vec<u8> = match e.link {
            Some(target) => target.as_bytes().to_vec(),
            None => e.data.to_vec(),
        };
        let folder = e.name.ends_with('/');
        let (method, packed): (u16, Vec<u8>) = if folder || e.link.is_some() {
            (0, raw.clone())
        } else {
            let mut d = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
            d.write_all(&raw).unwrap();
            (8, d.finish().unwrap())
        };
        let crc = crc32(&raw);
        let offset = out.len() as u32;
        let name = e.name.as_bytes();
        // Local file header.
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0x21, 0x5b]); // time, date
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(&packed);
        // Central directory entry: made on Unix (3), with the file's mode.
        let kind: u32 = if e.link.is_some() {
            0o120000
        } else if folder {
            0o040000
        } else {
            0o100000
        };
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&((3u16 << 8) | 20).to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&method.to_le_bytes());
        central.extend_from_slice(&[0, 0, 0x21, 0x5b]);
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        central.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0u8; 8]); // extra, comment, disk, internal attrs
        central.extend_from_slice(&((kind | e.mode) << 16).to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name);
    }
    let start = out.len() as u32;
    let count = entries.len() as u16;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0u8; 4]);
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}
