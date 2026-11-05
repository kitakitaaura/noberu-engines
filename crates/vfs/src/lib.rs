//! Random-access reads over game files.
//!
//! Engines never load whole archives. They read the index once, then pull
//! single entries by range. Native builds read `std::fs::File` directly; the
//! browser build will implement [`RangeRead`] over a `File`/`Blob` in a worker
//! (FileReaderSync), so the same parsers run unchanged.

use std::io;
use std::sync::Arc;

/// A byte source that can be read at any offset.
pub trait RangeRead: Send + Sync {
    /// Total size in bytes.
    fn len(&self) -> u64;

    /// Fill `buf` from `offset`. Errors if the range runs past the end.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Read `len` bytes at `offset` into a new buffer.
    fn read_vec(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        self.read_at(offset, &mut buf)?;
        Ok(buf)
    }
}

fn check_range(offset: u64, len: usize, total: u64) -> io::Result<()> {
    match offset.checked_add(len as u64) {
        Some(end) if end <= total => Ok(()),
        _ => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("range {offset}+{len} past end {total}"),
        )),
    }
}

impl RangeRead for [u8] {
    fn len(&self) -> u64 {
        <[u8]>::len(self) as u64
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        check_range(offset, buf.len(), RangeRead::len(self))?;
        let start = offset as usize;
        buf.copy_from_slice(&self[start..start + buf.len()]);
        Ok(())
    }
}

impl RangeRead for Vec<u8> {
    fn len(&self) -> u64 {
        self.as_slice().len() as u64
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.as_slice().read_at(offset, buf)
    }
}

impl<T: RangeRead + ?Sized> RangeRead for Arc<T> {
    fn len(&self) -> u64 {
        (**self).len()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        (**self).read_at(offset, buf)
    }
}

/// A native file opened for positional reads.
#[cfg(not(target_arch = "wasm32"))]
pub struct NativeFile {
    file: std::fs::File,
    len: u64,
}

#[cfg(not(target_arch = "wasm32"))]
impl NativeFile {
    pub fn open(path: impl AsRef<std::path::Path>) -> io::Result<Self> {
        let file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self { file, len })
    }
}

#[cfg(unix)]
impl RangeRead for NativeFile {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        use std::os::unix::fs::FileExt;
        check_range(offset, buf.len(), self.len)?;
        self.file.read_exact_at(buf, offset)
    }
}

#[cfg(windows)]
impl RangeRead for NativeFile {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        use std::os::windows::fs::FileExt;
        check_range(offset, buf.len(), self.len)?;
        let mut done = 0;
        while done < buf.len() {
            let n = self.file.seek_read(&mut buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            done += n;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_reads_and_bounds() {
        let data: Vec<u8> = (0..10).collect();
        assert_eq!(data.read_vec(3, 4).unwrap(), vec![3, 4, 5, 6]);
        assert!(data.read_vec(8, 3).is_err());
        assert!(data.read_vec(u64::MAX, 1).is_err());
    }
}
