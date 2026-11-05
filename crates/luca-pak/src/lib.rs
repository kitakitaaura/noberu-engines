//! LUCA System PAK archives, read lazily.
//!
//! Layout (all little-endian u32), as worked out by lbee-utils:
//!
//! ```text
//! header   data_offset entry_count id_start block_size
//!          subdir_offset unknown2 unknown3 unknown4 flags
//! pre      1..7 extra u32s, count from flags & 0b111
//! table    entry_count x (block_offset, length)
//! unk1     entry_count x 12 bytes        if flags & 0x100
//! names    [subdir\0] entry_count x name\0  if flags & 0x200
//! ...      padding up to data_offset
//! data     entry i at block_offset * block_size
//! ```
//!
//! Only the bytes before `data_offset` are read when opening.

use std::collections::HashMap;
use std::io;
use thiserror::Error;
use vfs::RangeRead;

const HEADER_LEN: usize = 9 * 4;
/// Sanity limit on the index size we will read up front.
const MAX_INDEX_BYTES: u32 = 64 << 20;
const MAX_ENTRIES: u32 = 1_000_000;

#[derive(Error, Debug)]
pub enum PakError {
    #[error("I/O: {0}")]
    Io(#[from] io::Error),
    #[error("malformed PAK header: {0}")]
    Header(String),
    #[error("no entry named {0:?}")]
    NotFound(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PakFlags(pub u32);

impl PakFlags {
    pub fn has_unknown_data1(self) -> bool {
        self.0 & 0x100 != 0
    }
    pub fn has_names(self) -> bool {
        self.0 & 0x200 != 0
    }
    pub fn extra_pre_count(self) -> usize {
        match self.0 & 0b111 {
            0 => 1,
            1 => 2,
            2 => 4,
            3 => 5,
            4 => 7,
            _ => 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Header {
    pub data_offset: u32,
    pub entry_count: u32,
    pub id_start: u32,
    pub block_size: u32,
    pub subdir_offset: u32,
    pub unknown: [u32; 3],
    pub flags: PakFlags,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub index: usize,
    /// Numeric id scripts use (`id_start + index`).
    pub id: u32,
    pub name: Option<String>,
    /// Absolute byte offset in the archive.
    pub offset: u64,
    pub length: u32,
    pub unknown1: Option<[u8; 12]>,
}

pub struct Pak<R: RangeRead> {
    src: R,
    pub header: Header,
    pub subdirectory: Option<String>,
    entries: Vec<Entry>,
    by_name: HashMap<String, usize>,
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn u32(&mut self) -> Result<u32, PakError> {
        let b = self
            .buf
            .get(self.pos..self.pos + 4)
            .ok_or_else(|| PakError::Header(format!("index truncated at {}", self.pos)))?;
        self.pos += 4;
        Ok(u32::from_le_bytes(b.try_into().unwrap()))
    }
    fn bytes<const N: usize>(&mut self) -> Result<[u8; N], PakError> {
        let b = self
            .buf
            .get(self.pos..self.pos + N)
            .ok_or_else(|| PakError::Header(format!("index truncated at {}", self.pos)))?;
        self.pos += N;
        Ok(b.try_into().unwrap())
    }
    fn cstr(&mut self) -> Result<String, PakError> {
        let rest = &self.buf[self.pos.min(self.buf.len())..];
        let end = rest
            .iter()
            .position(|&c| c == 0)
            .ok_or_else(|| PakError::Header("unterminated name".into()))?;
        self.pos += end + 1;
        // Names are ASCII in the titles seen so far; Shift-JIS would be lossy here.
        Ok(String::from_utf8_lossy(&rest[..end]).into_owned())
    }
}

impl<R: RangeRead> Pak<R> {
    /// Read the index. Entry data is not touched.
    pub fn open(src: R) -> Result<Self, PakError> {
        let mut head = [0u8; HEADER_LEN];
        src.read_at(0, &mut head)?;
        let mut c = Cursor { buf: &head, pos: 0 };
        let header = Header {
            data_offset: c.u32()?,
            entry_count: c.u32()?,
            id_start: c.u32()?,
            block_size: c.u32()?,
            subdir_offset: c.u32()?,
            unknown: [c.u32()?, c.u32()?, c.u32()?],
            flags: PakFlags(c.u32()?),
        };
        if header.entry_count > MAX_ENTRIES {
            return Err(PakError::Header(format!("{} entries", header.entry_count)));
        }
        if header.data_offset as usize <= HEADER_LEN
            || header.data_offset > MAX_INDEX_BYTES
            || header.data_offset as u64 > src.len()
        {
            return Err(PakError::Header(format!("data_offset {}", header.data_offset)));
        }
        if header.block_size == 0 {
            return Err(PakError::Header("block_size 0".into()));
        }

        let index = src.read_vec(0, header.data_offset as usize)?;
        let mut c = Cursor { buf: &index, pos: HEADER_LEN };
        for _ in 0..header.flags.extra_pre_count() {
            c.u32()?;
        }

        let n = header.entry_count as usize;
        let mut table = Vec::with_capacity(n);
        for _ in 0..n {
            table.push((c.u32()?, c.u32()?));
        }
        let unknown1: Option<Vec<[u8; 12]>> = if header.flags.has_unknown_data1() {
            Some((0..n).map(|_| c.bytes::<12>()).collect::<Result<_, _>>()?)
        } else {
            None
        };
        let mut subdirectory = None;
        let names: Option<Vec<String>> = if header.flags.has_names() {
            if header.subdir_offset != 0 {
                subdirectory = Some(c.cstr()?);
            }
            Some((0..n).map(|_| c.cstr()).collect::<Result<_, _>>()?)
        } else {
            None
        };

        let total = src.len();
        let mut entries = Vec::with_capacity(n);
        let mut by_name = HashMap::with_capacity(if names.is_some() { n } else { 0 });
        for (i, &(block, length)) in table.iter().enumerate() {
            let offset = block as u64 * header.block_size as u64;
            if offset + length as u64 > total {
                return Err(PakError::Header(format!("entry {i} runs past end of file")));
            }
            let name = names.as_ref().map(|v| v[i].clone());
            if let Some(nm) = &name {
                by_name.entry(nm.to_ascii_uppercase()).or_insert(i);
            }
            entries.push(Entry {
                index: i,
                id: header.id_start.wrapping_add(i as u32),
                name,
                offset,
                length,
                unknown1: unknown1.as_ref().map(|v| v[i]),
            });
        }

        Ok(Self { src, header, subdirectory, entries, by_name })
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Look up by name, case-insensitive (scripts and logs differ in case).
    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.by_name.get(&name.to_ascii_uppercase()).map(|&i| &self.entries[i])
    }

    /// Look up by the numeric id scripts use.
    pub fn by_id(&self, id: u32) -> Option<&Entry> {
        let i = id.checked_sub(self.header.id_start)? as usize;
        self.entries.get(i)
    }

    /// Read one entry's bytes.
    pub fn read(&self, entry: &Entry) -> Result<Vec<u8>, PakError> {
        Ok(self.src.read_vec(entry.offset, entry.length as usize)?)
    }

    pub fn read_named(&self, name: &str) -> Result<Vec<u8>, PakError> {
        let e = self.find(name).ok_or_else(|| PakError::NotFound(name.into()))?;
        self.read(e)
    }
}
