//! PMTiles v3 archive: <https://github.com/protomaps/PMTiles/blob/main/spec/v3/spec.md>.
//! Reads from local file or any byte-range source (HTTP for region extracts); writes
//! extracts with uncompressed directories. Supports uncompressed or gzip input.
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Write},
    os::unix::fs::FileExt,
    path::Path,
    sync::{Arc, Mutex},
};

const HEADER_LEN: usize = 127;
/// Spec: header plus root directory fit in first 16 KiB.
const ROOT_LIMIT: usize = 16_384 - HEADER_LEN;
/// Leaf directories are small; bound cache so long sessions don't grow without limit.
const LEAF_CACHE: usize = 64;

/// Reads `length` bytes at `offset`.
pub type Source = Box<dyn Fn(u64, u64) -> Result<Vec<u8>, String> + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Entry {
    tile_id: u64,
    offset: u64,
    length: u32,
    run_length: u32,
}

pub struct Archive {
    read: Source,
    header: [u8; HEADER_LEN],
    root: Arc<Vec<Entry>>,
    leaf_offset: u64,
    data_offset: u64,
    internal_compression: u8,
    tile_compression: u8,
    pub max_zoom: u8,
    /// Longitude/latitude of archive's suggested center.
    pub center: [f64; 2],
    /// Min lon, min lat, max lon, max lat.
    pub bounds: [f64; 4],
    leaves: Mutex<HashMap<u64, Arc<Vec<Entry>>>>,
}

impl Archive {
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("Open map: {e}"))?;
        Self::from_source(Box::new(move |offset, length| {
            let mut bytes = vec![0; length as usize];
            file.read_exact_at(&mut bytes, offset)
                .map_err(|e| format!("Read map: {e}"))?;
            Ok(bytes)
        }))
    }

    pub fn from_source(read: Source) -> Result<Self, String> {
        let bytes = read(0, HEADER_LEN as u64).map_err(|_| "Map file is not PMTiles")?;
        let header: [u8; HEADER_LEN] = bytes.try_into().map_err(|_| "Map file is not PMTiles")?;
        if &header[..7] != b"PMTiles" || header[7] != 3 {
            return Err("Map file is not PMTiles v3".into());
        }
        if header[99] != 1 {
            return Err("Map file must contain vector (MVT) tiles".into());
        }
        let mut archive = Self {
            read,
            header,
            root: Arc::new(Vec::new()),
            leaf_offset: u64_at(&header, 40),
            data_offset: u64_at(&header, 56),
            internal_compression: header[97],
            tile_compression: header[98],
            max_zoom: header[101],
            center: [
                f64::from(i32_at(&header, 119)) / 1e7,
                f64::from(i32_at(&header, 123)) / 1e7,
            ],
            bounds: [102, 106, 110, 114].map(|i| f64::from(i32_at(&header, i)) / 1e7),
            leaves: Mutex::new(HashMap::new()),
        };
        archive.root = Arc::new(archive.directory(u64_at(&header, 8), u64_at(&header, 16))?);
        Ok(archive)
    }

    /// Absolute byte range of tile data, or None when archive lacks tile.
    pub fn entry(&self, z: u8, x: u32, y: u32) -> Result<Option<(u64, u32)>, String> {
        let id = tile_id(z, x, y);
        let mut directory = self.root.clone();
        // Spec bounds directory depth at 3 levels below root.
        for _ in 0..4 {
            let Some(entry) = find(&directory, id) else {
                return Ok(None);
            };
            if entry.run_length > 0 {
                return Ok(Some((self.data_offset + entry.offset, entry.length)));
            }
            directory = self.leaf(entry.offset, u64::from(entry.length))?;
        }
        Err("Map directory too deep".into())
    }

    /// Decompressed tile bytes, or None when archive lacks tile.
    pub fn tile(&self, z: u8, x: u32, y: u32) -> Result<Option<Vec<u8>>, String> {
        let Some((offset, length)) = self.entry(z, x, y)? else {
            return Ok(None);
        };
        decompress(self.read(offset, u64::from(length))?, self.tile_compression).map(Some)
    }

    /// Every tile as (id, absolute offset, length), runs expanded, sorted by id.
    pub fn entries(&self) -> Result<Vec<(u64, u64, u32)>, String> {
        let mut out = Vec::new();
        let mut stack = vec![(self.root.clone(), 0)];
        while let Some((directory, depth)) = stack.pop() {
            for e in directory.iter() {
                if e.run_length == 0 {
                    if depth >= 3 {
                        return Err("Map directory too deep".into());
                    }
                    stack.push((self.leaf(e.offset, u64::from(e.length))?, depth + 1));
                } else {
                    for i in 0..u64::from(e.run_length) {
                        out.push((e.tile_id + i, self.data_offset + e.offset, e.length));
                    }
                }
            }
        }
        out.sort_unstable();
        Ok(out)
    }

    /// Same tile encoding, so tiles can be copied between archives.
    pub fn compatible(&self, other: &Self) -> bool {
        self.tile_compression == other.tile_compression && self.header[99] == other.header[99]
    }

    fn leaf(&self, offset: u64, length: u64) -> Result<Arc<Vec<Entry>>, String> {
        if let Some(leaf) = self.leaves.lock().unwrap().get(&offset) {
            return Ok(leaf.clone());
        }
        let leaf = Arc::new(self.directory(self.leaf_offset + offset, length)?);
        let mut leaves = self.leaves.lock().unwrap();
        if leaves.len() >= LEAF_CACHE {
            leaves.clear();
        }
        leaves.insert(offset, leaf.clone());
        Ok(leaf)
    }

    pub fn read(&self, offset: u64, length: u64) -> Result<Vec<u8>, String> {
        if length > 64 << 20 {
            return Err("Map entry too large".into());
        }
        (self.read)(offset, length)
    }

    fn directory(&self, offset: u64, length: u64) -> Result<Vec<Entry>, String> {
        let bytes = decompress(self.read(offset, length)?, self.internal_compression)?;
        parse_directory(&bytes)
    }
}

/// Writes extract archive. `tiles` holds (tile id, offset, length) into `data`, sorted by
/// id; equal offsets share content. Copies tile format from `source`.
pub fn write(
    out: &Path,
    source: &Archive,
    tiles: &[(u64, u64, u32)],
    data: &Path,
    zooms: (u8, u8),
    bounds: [f64; 4],
) -> Result<(), String> {
    let mut entries: Vec<Entry> = Vec::new();
    for &(tile_id, offset, length) in tiles {
        match entries.last_mut() {
            Some(last)
                if last.tile_id + u64::from(last.run_length) == tile_id
                    && last.offset == offset =>
            {
                last.run_length += 1;
            }
            _ => entries.push(Entry {
                tile_id,
                offset,
                length,
                run_length: 1,
            }),
        }
    }
    let (root, leaves) = directories(&entries);
    let metadata = b"{}";
    let data_len = std::fs::metadata(data).map_err(|e| e.to_string())?.len();
    let mut header = [0_u8; HEADER_LEN];
    header[..7].copy_from_slice(b"PMTiles");
    header[7] = 3;
    let root_offset = HEADER_LEN as u64;
    let metadata_offset = root_offset + root.len() as u64;
    let leaf_offset = metadata_offset + metadata.len() as u64;
    let data_offset = leaf_offset + leaves.len() as u64;
    let unique = {
        let mut offsets: Vec<u64> = tiles.iter().map(|t| t.1).collect();
        offsets.sort_unstable();
        offsets.dedup();
        offsets.len() as u64
    };
    for (i, value) in [
        root_offset,
        root.len() as u64,
        metadata_offset,
        metadata.len() as u64,
        leaf_offset,
        leaves.len() as u64,
        data_offset,
        data_len,
        tiles.len() as u64,
        entries.len() as u64,
        unique,
    ]
    .into_iter()
    .enumerate()
    {
        header[8 + i * 8..16 + i * 8].copy_from_slice(&value.to_le_bytes());
    }
    header[96] = 0; // not clustered
    header[97] = 1; // directories uncompressed
    header[98] = source.tile_compression;
    header[99] = source.header[99];
    header[100] = zooms.0;
    header[101] = zooms.1;
    let e7 = |v: f64| ((v * 1e7) as i32).to_le_bytes();
    for (i, v) in bounds.iter().enumerate() {
        header[102 + i * 4..106 + i * 4].copy_from_slice(&e7(*v));
    }
    header[118] = zooms.1.min(14);
    header[119..123].copy_from_slice(&e7((bounds[0] + bounds[2]) / 2.0));
    header[123..127].copy_from_slice(&e7((bounds[1] + bounds[3]) / 2.0));
    let mut file = File::create(out).map_err(|e| e.to_string())?;
    for part in [&header[..], &root, metadata, &leaves] {
        file.write_all(part).map_err(|e| e.to_string())?;
    }
    std::io::copy(&mut File::open(data).map_err(|e| e.to_string())?, &mut file)
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())
}

/// Root directory bytes and leaf section bytes; leaves only when root exceeds limit.
fn directories(entries: &[Entry]) -> (Vec<u8>, Vec<u8>) {
    let root = serialize(entries);
    if root.len() <= ROOT_LIMIT {
        return (root, Vec::new());
    }
    let mut size = 4096;
    loop {
        let mut leaves = Vec::new();
        let mut pointers = Vec::new();
        for chunk in entries.chunks(size) {
            let bytes = serialize(chunk);
            pointers.push(Entry {
                tile_id: chunk[0].tile_id,
                offset: leaves.len() as u64,
                length: bytes.len() as u32,
                run_length: 0,
            });
            leaves.extend(bytes);
        }
        let root = serialize(&pointers);
        if root.len() <= ROOT_LIMIT {
            return (root, leaves);
        }
        size *= 2;
    }
}

fn serialize(entries: &[Entry]) -> Vec<u8> {
    let mut out = Vec::new();
    write_varint(&mut out, entries.len() as u64);
    let mut last = 0;
    for e in entries {
        write_varint(&mut out, e.tile_id - last);
        last = e.tile_id;
    }
    for e in entries {
        write_varint(&mut out, u64::from(e.run_length));
    }
    for e in entries {
        write_varint(&mut out, u64::from(e.length));
    }
    for (i, e) in entries.iter().enumerate() {
        let contiguous =
            i > 0 && e.offset == entries[i - 1].offset + u64::from(entries[i - 1].length);
        write_varint(&mut out, if contiguous { 0 } else { e.offset + 1 });
    }
    out
}

fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push(value as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn u64_at(header: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(header[i..i + 8].try_into().unwrap())
}

fn i32_at(header: &[u8], i: usize) -> i32 {
    i32::from_le_bytes(header[i..i + 4].try_into().unwrap())
}

fn decompress(bytes: Vec<u8>, compression: u8) -> Result<Vec<u8>, String> {
    match compression {
        0 | 1 => Ok(bytes),
        2 => {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(&bytes[..])
                .read_to_end(&mut out)
                .map_err(|e| format!("Map gzip: {e}"))?;
            Ok(out)
        }
        _ => Err("Map compression unsupported; use gzip PMTiles".into()),
    }
}

fn varint(bytes: &[u8], pos: &mut usize) -> Result<u64, String> {
    let mut value = 0_u64;
    for shift in (0..64).step_by(7) {
        let byte = *bytes.get(*pos).ok_or("Map directory truncated")?;
        *pos += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte < 0x80 {
            return Ok(value);
        }
    }
    Err("Map varint overflow".into())
}

fn parse_directory(bytes: &[u8]) -> Result<Vec<Entry>, String> {
    let pos = &mut 0;
    let n = varint(bytes, pos)? as usize;
    if n > bytes.len() {
        return Err("Map directory corrupt".into());
    }
    let mut entries = vec![
        Entry {
            tile_id: 0,
            offset: 0,
            length: 0,
            run_length: 0
        };
        n
    ];
    let mut last = 0;
    for e in &mut entries {
        last += varint(bytes, pos)?;
        e.tile_id = last;
    }
    for e in &mut entries {
        e.run_length = varint(bytes, pos)? as u32;
    }
    for e in &mut entries {
        e.length = varint(bytes, pos)? as u32;
    }
    for i in 0..n {
        let value = varint(bytes, pos)?;
        entries[i].offset = if value == 0 && i > 0 {
            entries[i - 1].offset + u64::from(entries[i - 1].length)
        } else {
            value.wrapping_sub(1)
        };
    }
    Ok(entries)
}

/// Last entry with tile_id <= id that covers id (run) or points at leaf (run_length 0).
fn find(entries: &[Entry], id: u64) -> Option<Entry> {
    let i = entries
        .partition_point(|e| e.tile_id <= id)
        .checked_sub(1)?;
    let e = entries[i];
    (e.run_length == 0 || id - e.tile_id < u64::from(e.run_length)).then_some(e)
}

/// Hilbert-curve tile id: tiles of lower zooms first, then position along curve.
pub fn tile_id(z: u8, mut x: u32, mut y: u32) -> u64 {
    let mut id = ((1_u64 << (2 * u32::from(z))) - 1) / 3;
    for a in (0..u32::from(z)).rev() {
        let s = 1_u32 << a;
        let rx = u64::from(x & s != 0);
        let ry = u64::from(y & s != 0);
        id += ((3 * rx) ^ ry) << (2 * a);
        if ry == 0 {
            if rx == 1 {
                x = s.wrapping_sub(1).wrapping_sub(x);
                y = s.wrapping_sub(1).wrapping_sub(y);
            }
            std::mem::swap(&mut x, &mut y);
        }
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn fixture() -> Archive {
        Archive::open(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/downtown.pmtiles"),
        )
        .unwrap()
    }

    #[test]
    fn tile_ids_follow_spec() {
        assert_eq!(tile_id(0, 0, 0), 0);
        assert_eq!(tile_id(1, 0, 0), 1);
        assert_eq!(tile_id(1, 0, 1), 2);
        assert_eq!(tile_id(1, 1, 1), 3);
        assert_eq!(tile_id(1, 1, 0), 4);
        assert_eq!(tile_id(2, 0, 0), 5);
        assert_eq!(tile_id(20, 0, 0), 366503875925);
    }

    #[test]
    fn directory_round_trip_and_lookup() {
        // 2 entries: ids 5 (run 2) and 9 (leaf), lengths 10 and 20, offsets 0 and 10 (implicit).
        let bytes = [2, 5, 4, 2, 0, 10, 20, 1, 0];
        let entries = parse_directory(&bytes).unwrap();
        assert_eq!(entries[1].offset, 10);
        assert_eq!(serialize(&entries), bytes);
        assert_eq!(find(&entries, 6).unwrap().tile_id, 5);
        assert!(find(&entries, 7).is_none());
        assert_eq!(find(&entries, 50).unwrap().run_length, 0);
        assert!(find(&entries, 4).is_none());
    }

    #[test]
    fn written_extract_reads_back_with_leaves() {
        let source = fixture();
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let tile = source.tile(15, 5217, 11720).unwrap().unwrap();
        let (offset, length) = source.entry(15, 5217, 11720).unwrap().unwrap();
        let raw = source.read(offset, u64::from(length)).unwrap();
        std::fs::write(&data, &raw).unwrap();
        // Many ids sharing one tile, spaced so runs can't merge: forces leaf directories.
        let id = tile_id(15, 5217, 11720);
        let mut tiles: Vec<(u64, u64, u32)> = (0..6000).map(|i| (i * 2, 0, length)).collect();
        tiles.push((id, 0, length));
        tiles.sort_unstable();
        let out = dir.path().join("out.pmtiles");
        write(
            &out,
            &source,
            &tiles,
            &data,
            (0, 15),
            [-1.0, -1.0, 1.0, 1.0],
        )
        .unwrap();
        let written = Archive::open(&out).unwrap();
        assert!(u64_at(&written.header, 48) > 0, "expected leaf directories");
        assert_eq!(written.entries().unwrap().len(), tiles.len());
        assert_eq!(written.bounds, [-1.0, -1.0, 1.0, 1.0]);
        assert_eq!(written.tile(15, 5217, 11720).unwrap().unwrap(), tile);
        assert!(written.tile(15, 5217, 11721).unwrap().is_none());
    }
}
