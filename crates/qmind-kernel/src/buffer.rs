//! Buffer pool — page cache between index/heap code and the [`PageStore`].
//!
//! M1 scope: single-threaded, write-back, clock-sweep eviction. Every load
//! from the store is CRC-validated (corruption detected at read). Callers
//! mutate only the payload region via [`BufferPool::payload_mut`] so sealed
//! headers stay checksum-consistent. Concurrency arrives with M2.

use crate::error::{Error, Result};
use crate::page::{PageHeader, PageId, PAGE_SIZE};
use std::collections::HashMap;

/// Durable page storage. M1 ships RAM + file-backed stores; segment layout
/// is versioned per D-003.
pub trait PageStore {
    fn read_page(&mut self, id: PageId, buf: &mut [u8]) -> Result<()>;
    fn write_page(&mut self, id: PageId, buf: &[u8]) -> Result<()>;
    /// Durability barrier for all previously written pages.
    fn sync(&mut self) -> Result<()>;
    fn contains(&mut self, id: PageId) -> bool;
}

/// In-memory store for tests and benchmarks.
#[derive(Default)]
pub struct RamPageStore {
    pages: HashMap<PageId, Box<[u8; PAGE_SIZE]>>,
}

impl RamPageStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl PageStore for RamPageStore {
    fn read_page(&mut self, id: PageId, buf: &mut [u8]) -> Result<()> {
        let src = self
            .pages
            .get(&id)
            .ok_or(Error::PageNotFound { page: id })?;
        buf.copy_from_slice(src.as_ref());
        Ok(())
    }

    fn write_page(&mut self, id: PageId, buf: &[u8]) -> Result<()> {
        let dst = self
            .pages
            .entry(id)
            .or_insert_with(|| Box::new([0u8; PAGE_SIZE]));
        dst.copy_from_slice(buf);
        Ok(())
    }

    fn sync(&mut self) -> Result<()> {
        Ok(())
    }

    fn contains(&mut self, id: PageId) -> bool {
        self.pages.contains_key(&id)
    }
}

type FrameId = usize;

struct Frame {
    page_id: PageId,
    data: Box<[u8; PAGE_SIZE]>,
    pin_count: u32,
    ref_bit: bool,
    dirty: bool,
}

impl Frame {
    fn zeroed(page_id: PageId) -> Self {
        let mut data = Box::new([0u8; PAGE_SIZE]);
        PageHeader::new(page_id).encode_into(data.as_mut());
        Self {
            page_id,
            data,
            pin_count: 0,
            ref_bit: true,
            dirty: false,
        }
    }

    /// Re-seal the full-page checksum over current contents. The pool owns
    /// the header (it lives inside `data`), so this runs before every durable
    /// write of a dirty frame.
    fn seal(&mut self) {
        PageHeader::new(self.page_id).encode_into(self.data.as_mut());
    }
}

pub struct BufferPool<S: PageStore> {
    store: S,
    capacity: usize,
    frames: Vec<Frame>,
    map: HashMap<PageId, FrameId>,
    hand: usize,
}

impl BufferPool<RamPageStore> {
    pub fn in_memory(capacity: usize) -> Self {
        Self::new(RamPageStore::new(), capacity)
    }
}

impl<S: PageStore> BufferPool<S> {
    pub fn new(store: S, capacity: usize) -> Self {
        assert!(capacity > 0, "buffer pool capacity must be positive");
        Self {
            store,
            capacity,
            frames: Vec::with_capacity(capacity),
            map: HashMap::with_capacity(capacity),
            hand: 0,
        }
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    /// Consume the pool and reclaim its durable store (simulated restart).
    pub fn store_owned(self) -> S {
        self.store
    }

    pub fn resident_pages(&self) -> usize {
        self.frames.len()
    }

    pub fn contains(&self, id: PageId) -> bool {
        self.map.contains_key(&id)
    }

    /// Create a fresh zeroed page in a pinned frame. Errors if `id` already
    /// exists in pool or store.
    pub fn create_page(&mut self, id: PageId) -> Result<FrameId> {
        if self.map.contains_key(&id) || self.store.contains(id) {
            return Err(Error::Other(format!("page {id} already exists")));
        }
        self.make_room()?;
        let fid = self.frames.len();
        self.frames.push(Frame::zeroed(id));
        self.frames[fid].pin_count = 1;
        self.frames[fid].dirty = true;
        self.map.insert(id, fid);
        Ok(fid)
    }

    /// Pin an existing page, loading it through the store (CRC-validated).
    pub fn pin(&mut self, id: PageId) -> Result<FrameId> {
        if let Some(&fid) = self.map.get(&id) {
            let f = &mut self.frames[fid];
            f.pin_count += 1;
            f.ref_bit = true;
            return Ok(fid);
        }
        if !self.store.contains(id) {
            return Err(Error::PageNotFound { page: id });
        }
        let fid = self.frames.len();
        let mut data = Box::new([0u8; PAGE_SIZE]);
        self.store.read_page(id, data.as_mut())?;
        // Validate header on every load — corruption never propagates silently.
        PageHeader::decode(data.as_ref())?;
        let frame = Frame {
            page_id: id,
            data,
            pin_count: 1,
            ref_bit: true,
            dirty: false,
        };
        self.frames.push(frame);
        self.map.insert(id, fid);
        Ok(fid)
    }

    pub fn unpin(&mut self, fid: FrameId, dirty: bool) {
        let f = &mut self.frames[fid];
        f.pin_count = f.pin_count.saturating_sub(1);
        if dirty {
            f.dirty = true;
        }
    }

    /// Read-only view of the whole page (header + payload).
    pub fn bytes(&self, fid: FrameId) -> &[u8] {
        self.frames[fid].data.as_ref()
    }

    /// Mutable view restricted to the payload region — header integrity is
    /// owned by the pool.
    pub fn payload_mut(&mut self, fid: FrameId) -> &mut [u8] {
        self.frames[fid].dirty = true;
        &mut self.frames[fid].data[PageHeader::SIZE..]
    }

    pub fn payload(&self, fid: FrameId) -> &[u8] {
        &self.frames[fid].data[PageHeader::SIZE..]
    }

    /// Write back all dirty frames and sync the store. Returns count flushed.
    pub fn flush_all(&mut self) -> Result<usize> {
        let mut n = 0;
        for f in self.frames.iter_mut() {
            if f.dirty {
                f.seal();
                self.store.write_page(f.page_id, f.data.as_ref())?;
                f.dirty = false;
                n += 1;
            }
        }
        self.store.sync()?;
        Ok(n)
    }

    /// Ensure one append-slot is available: if at capacity, evict via clock
    /// sweep. Eviction shrinks the frame vec; callers always `push` after this.
    fn make_room(&mut self) -> Result<()> {
        if self.frames.len() < self.capacity {
            return Ok(());
        }
        let cap = self.frames.len();
        let mut victim = None;
        for _ in 0..(2 * cap) {
            let idx = self.hand % cap;
            if self.frames[idx].pin_count == 0 && !self.frames[idx].ref_bit {
                victim = Some(idx);
                break;
            }
            if self.frames[idx].pin_count == 0 {
                self.frames[idx].ref_bit = false;
            }
            self.hand = idx + 1;
        }
        let v = victim.ok_or(Error::NoEvictableFrame)?;
        let mut frame = self.frames.swap_remove(v);
        self.map.remove(&frame.page_id);
        // swap_remove relocated the tail frame into slot `v` — re-point it.
        if v < self.frames.len() {
            let moved = self.frames[v].page_id;
            self.map.insert(moved, v);
        }
        if frame.dirty {
            frame.seal();
            self.store.write_page(frame.page_id, frame.data.as_ref())?;
        }
        if !self.frames.is_empty() {
            self.hand %= self.frames.len();
        } else {
            self.hand = 0;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P0: PageId = 10;

    #[test]
    fn create_pin_roundtrip_preserves_payload() {
        let mut pool = BufferPool::in_memory(4);
        let f = pool.create_page(P0).unwrap();
        pool.payload_mut(f)[0..4].copy_from_slice(&0xABCD_u32.to_le_bytes());
        pool.unpin(f, true);
        pool.flush_all().unwrap();

        let f = pool.pin(P0).unwrap();
        assert_eq!(
            &pool.bytes(f)[PageHeader::SIZE..PageHeader::SIZE + 4],
            &0xABCD_u32.to_le_bytes()
        );
        pool.unpin(f, false);
    }

    #[test]
    fn eviction_is_write_back_not_write_through() {
        let mut pool = BufferPool::in_memory(2);
        let a = pool.create_page(1).unwrap();
        let b = pool.create_page(2).unwrap();
        pool.payload_mut(a)[0] = 7;
        pool.payload_mut(b)[0] = 9;
        pool.unpin(a, true);
        pool.unpin(b, true);

        // Third page forces eviction of one dirty page — must survive via store.
        let c = pool.create_page(3).unwrap();
        pool.payload_mut(c)[0] = 11;
        pool.unpin(c, true);

        let fa = pool.pin(1).unwrap();
        assert_eq!(pool.payload(fa)[0], 7);
        let fb = pool.pin(2).unwrap();
        assert_eq!(pool.payload(fb)[0], 9);
        let fc = pool.pin(3).unwrap();
        assert_eq!(pool.payload(fc)[0], 11);
        for f in [fa, fb, fc] {
            pool.unpin(f, false);
        }
    }

    #[test]
    fn all_pinned_exhausts_pool() {
        let mut pool = BufferPool::in_memory(2);
        let a = pool.create_page(1).unwrap();
        let _b = pool.create_page(2).unwrap();
        pool.create_page(3).expect_err("every frame pinned");
        pool.unpin(a, false);
        pool.create_page(3).expect("evictable after unpin");
    }

    #[test]
    fn corrupted_store_page_rejected_on_load() {
        // Capacity 1: creating a second page evicts P0 to the store, so the
        // later pin must round-trip through (corrupted) durable storage.
        let mut pool = BufferPool::in_memory(1);
        let f = pool.create_page(P0).unwrap();
        pool.unpin(f, true);
        let other = pool.create_page(999).unwrap();
        pool.unpin(other, true);
        pool.flush_all().unwrap();

        // Corrupt the durable copy behind the pool's back.
        let raw = pool.store_mut().pages.get_mut(&P0).unwrap();
        raw[5] ^= 0xFF;

        match pool.pin(P0) {
            Err(Error::ChecksumMismatch { .. }) => {}
            other => panic!("expected ChecksumMismatch, got {other:?}"),
        }
    }
}
