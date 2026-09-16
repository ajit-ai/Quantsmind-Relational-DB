//! File-backed [`PageStore`] — segmented page files with versioned headers.
//!
//! Layout per D-003: each segment file opens with
//! `[magic "QMINDSEG1"][u16 format_version][padding to 16 bytes]`,
//! followed by fixed 8 KiB page slots. Segment number = page_id / slots,
//! slot offset = (page_id % slots) * PAGE_SIZE + header size.

use crate::buffer::PageStore;
use crate::error::{Error, Result};
use crate::page::{PageId, FORMAT_VERSION, PAGE_SIZE};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const SEGMENT_MAGIC: &[u8; 8] = b"QMINDSEG";
/// Slots per segment file: 256 × 8 KiB = 2 MiB segments.
pub const PAGES_PER_SEGMENT: u64 = 256;
const HEADER_SIZE: u64 = 16;

#[derive(Debug)]
pub struct FilePageStore {
    dir: PathBuf,
    segments: std::collections::HashMap<u64, File>,
}

impl FilePageStore {
    /// Open (or initialize) a data directory. Existing segments are
    /// validated for magic + compatible format version.
    pub fn open<P: AsRef<Path>>(dir: P) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        let mut segments = std::collections::HashMap::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(seg) = name
                .strip_prefix("seg_")
                .and_then(|s| s.strip_suffix(".bin"))
            {
                let n: u64 = seg
                    .parse()
                    .map_err(|_| Error::Other(format!("unexpected file in data dir: {name}")))?;
                validate_segment(&entry.path())?;
                segments.insert(n, open_rw(&entry.path())?);
            }
        }
        Ok(Self { dir, segments })
    }

    fn seg_path(&self, seg: u64) -> PathBuf {
        self.dir.join(format!("seg_{seg:06}.bin"))
    }

    fn segment(&mut self, seg: u64) -> Result<&mut File> {
        if !self.segments.contains_key(&seg) {
            let path = self.seg_path(seg);
            if path.exists() {
                validate_segment(&path)?;
                self.segments.insert(seg, open_rw(&path)?);
            } else {
                let mut file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(&path)?;
                write_header(&mut file)?;
                self.segments.insert(seg, file);
            }
        }
        Ok(self.segments.get_mut(&seg).expect("segment just inserted"))
    }

    fn locate(page: PageId) -> Result<(u64, u64)> {
        if page == 0 {
            return Err(Error::Other("page id 0 is reserved".into()));
        }
        Ok((page / PAGES_PER_SEGMENT, page % PAGES_PER_SEGMENT))
    }

    pub fn data_dir(&self) -> &Path {
        &self.dir
    }
}

impl PageStore for FilePageStore {
    fn read_page(&mut self, id: PageId, buf: &mut [u8]) -> Result<()> {
        assert_eq!(buf.len(), PAGE_SIZE);
        let (seg, slot) = Self::locate(id)?;
        let file = self.segment(seg)?;
        let offset = HEADER_SIZE + slot * PAGE_SIZE as u64;
        let meta = file.metadata()?;
        if meta.len() < offset + PAGE_SIZE as u64 {
            return Err(Error::PageNotFound { page: id });
        }
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(buf.as_mut())?;
        Ok(())
    }

    fn write_page(&mut self, id: PageId, buf: &[u8]) -> Result<()> {
        assert_eq!(buf.len(), PAGE_SIZE);
        let (seg, slot) = Self::locate(id)?;
        let file = self.segment(seg)?;
        let offset = HEADER_SIZE + slot * PAGE_SIZE as u64;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(buf)?;
        Ok(())
    }

    fn sync(&mut self) -> Result<()> {
        for f in self.segments.values_mut() {
            f.sync_all()?;
        }
        Ok(())
    }

    fn contains(&mut self, id: PageId) -> bool {
        match Self::locate(id) {
            Ok((seg, slot)) => {
                let need = HEADER_SIZE + (slot + 1) * PAGE_SIZE as u64;
                self.seg_path(seg)
                    .metadata()
                    .map(|m| m.len() >= need)
                    .unwrap_or(false)
            }
            Err(_) => false,
        }
    }
}

fn open_rw(path: &Path) -> Result<File> {
    Ok(OpenOptions::new().read(true).write(true).open(path)?)
}

fn write_header(file: &mut File) -> Result<()> {
    let mut hdr = [0u8; HEADER_SIZE as usize];
    hdr[..8].copy_from_slice(SEGMENT_MAGIC);
    hdr[8..10].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&hdr)?;
    Ok(())
}

fn validate_segment(path: &Path) -> Result<()> {
    let mut f = File::open(path)?;
    let mut hdr = [0u8; HEADER_SIZE as usize];
    if f.read_exact(&mut hdr).is_err() || &hdr[..8] != SEGMENT_MAGIC {
        return Err(Error::Other(format!(
            "{}: not a QuantsMind segment (bad magic)",
            path.display()
        )));
    }
    let v = u16::from_le_bytes(hdr[8..10].try_into().unwrap());
    if v > FORMAT_VERSION {
        return Err(Error::Other(format!(
            "{}: segment format v{v} newer than engine v{FORMAT_VERSION}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::BufferPool;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("qmind_test_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn pages_roundtrip_through_real_files_across_restart() {
        let dir = temp_dir("roundtrip");
        {
            let mut pool = BufferPool::new(FilePageStore::open(&dir).unwrap(), 4);
            for id in 1..=600u64 {
                let f = pool.create_page(id).unwrap(); // spans 3 segments
                pool.payload_mut(f)[0..8].copy_from_slice(&(id * 7).to_le_bytes());
                pool.unpin(f, true);
            }
            pool.flush_all().unwrap();
        }
        // Simulated restart: brand-new store over same directory.
        let mut fresh = BufferPool::new(FilePageStore::open(&dir).unwrap(), 8);
        for id in [1u64, 256, 257, 512, 600] {
            let f = fresh.pin(id).unwrap();
            assert_eq!(&fresh.payload(f)[0..8], &(id * 7).to_le_bytes());
            fresh.unpin(f, false);
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn segments_carry_versioned_magic_headers() {
        let dir = temp_dir("headers");
        let mut pool = BufferPool::new(FilePageStore::open(&dir).unwrap(), 2);
        let f = pool.create_page(300).unwrap();
        pool.unpin(f, true);
        pool.flush_all().unwrap();

        // page 300 -> segment 1; header must be present and valid
        let p = dir.join("seg_000001.bin");
        validate_segment(&p).unwrap();
        let mut hdr = [0u8; 16];
        File::open(&p).unwrap().read_exact(&mut hdr).unwrap();
        assert_eq!(&hdr[..8], SEGMENT_MAGIC);

        // corrupting magic makes reopen fail loudly
        let mut raw = fs::OpenOptions::new().write(true).open(&p).unwrap();
        raw.seek(SeekFrom::Start(0)).unwrap();
        raw.write_all(b"XXXXXXXX").unwrap();
        drop(raw);
        assert!(FilePageStore::open(&dir).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn future_format_version_rejected_on_open() {
        let dir = temp_dir("futurever");
        let mut pool = BufferPool::new(FilePageStore::open(&dir).unwrap(), 2);
        let f = pool.create_page(5).unwrap();
        pool.unpin(f, true);
        pool.flush_all().unwrap();

        let p = dir.join("seg_000000.bin");
        let mut raw = fs::OpenOptions::new().write(true).open(&p).unwrap();
        raw.seek(SeekFrom::Start(8)).unwrap();
        raw.write_all(&(FORMAT_VERSION + 3).to_le_bytes()).unwrap();
        drop(raw);
        let err = FilePageStore::open(&dir).unwrap_err();
        assert!(err.to_string().contains("newer than engine"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupted_durable_bytes_fail_pin_with_checksum_error() {
        let dir = temp_dir("corrupt");
        let mut pool = BufferPool::new(FilePageStore::open(&dir).unwrap(), 1);
        let f = pool.create_page(9).unwrap();
        pool.payload_mut(f)[3] = 42;
        pool.unpin(f, true);
        pool.flush_all().unwrap();

        let (seg, slot) = FilePageStore::locate(9).unwrap();
        let p = dir.join(format!("seg_{seg:06}.bin"));
        let off = HEADER_SIZE + slot * PAGE_SIZE as u64;
        let mut raw = fs::OpenOptions::new().write(true).open(&p).unwrap();
        raw.seek(SeekFrom::Start(off + 100)).unwrap();
        raw.write_all(&[0xFF]).unwrap();
        drop(raw);

        let mut reopened = BufferPool::new(FilePageStore::open(&dir).unwrap(), 2);
        assert!(matches!(
            reopened.pin(9),
            Err(Error::ChecksumMismatch { .. })
        ));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reserved_page_zero_is_rejected() {
        let dir = temp_dir("zeropage");
        let mut store = FilePageStore::open(&dir).unwrap();
        let buf = vec![0u8; PAGE_SIZE];
        assert!(store.write_page(0, &buf).is_err());
        let _ = fs::remove_dir_all(dir);
    }
}
