//! Fixed-size pages — the unit of storage and I/O.
//!
//! M1: CRC-protected headers, heap row layout, split/merge accounting.

use crate::error::{Error, Result};

/// Page size in bytes. 8 KiB balances OLTP point-read locality against
/// vectorized scan I/O amplification (locked for M1; versioned format, D-003).
pub const PAGE_SIZE: usize = 8192;

pub type PageId = u64;

/// Wire-stable page header. Serialized little-endian; never reorder fields
/// without bumping `FORMAT_VERSION` (D-003: storage format is a contract).
/// A full in-memory page: typed header + raw payload region.
#[derive(Debug, Clone)]
pub struct Page {
    pub header: PageHeader,
    data: Box<[u8; PAGE_SIZE]>,
}

impl Page {
    pub fn new(page_id: PageId) -> Self {
        let mut data = Box::new([0u8; PAGE_SIZE]);
        PageHeader::new(page_id).encode_into(data.as_mut());
        Self {
            header: PageHeader::new(page_id),
            data,
        }
    }

    /// Payload region after the header.
    pub fn payload(&self) -> &[u8] {
        &self.data[PageHeader::SIZE..]
    }

    pub fn payload_mut(&mut self) -> &mut [u8] {
        &mut self.data[PageHeader::SIZE..]
    }

    /// Recompute header checksum over current contents; call before write-out.
    pub fn seal(&mut self) {
        self.header.encode_into(self.data.as_mut());
    }
}

impl std::ops::Deref for Page {
    type Target = [u8; PAGE_SIZE];
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageHeader {
    pub checksum: u32,
    pub page_id: PageId,
    /// On-disk layout version for this page's payload region.
    pub format_version: u16,
    pub flags: u16,
    /// Occupied bytes in payload area.
    pub used: u16,
}

impl PageHeader {
    /// Serialized little-endian with EXPLICIT offsets — never derive layout
    /// from repr(C) (alignment padding shifts the checksum slot). Never
    /// reorder fields without bumping `FORMAT_VERSION` (D-003).
    pub const OFF_PAGE_ID: usize = 0;
    pub const OFF_FORMAT_VERSION: usize = 8;
    pub const OFF_FLAGS: usize = 10;
    pub const OFF_USED: usize = 12;
    pub const OFF_CHECKSUM: usize = 14;
    /// 8 (page_id) + 2+2+2 (version, flags, used) + 4 (crc)
    pub const SIZE: usize = Self::OFF_CHECKSUM + 4;

    pub fn new(page_id: PageId) -> Self {
        Self {
            checksum: 0,
            page_id,
            format_version: FORMAT_VERSION,
            flags: 0,
            used: 0,
        }
    }

    /// Encode header into `buf[0..Self::SIZE]` and seal a checksum over the
    /// full page (header fields + payload), skipping the checksum slot.
    pub fn encode_into(&self, buf: &mut [u8]) {
        buf[Self::OFF_PAGE_ID..8].copy_from_slice(&self.page_id.to_le_bytes());
        buf[Self::OFF_FORMAT_VERSION..Self::OFF_FLAGS]
            .copy_from_slice(&self.format_version.to_le_bytes());
        buf[Self::OFF_FLAGS..Self::OFF_USED].copy_from_slice(&self.flags.to_le_bytes());
        buf[Self::OFF_USED..Self::OFF_CHECKSUM].copy_from_slice(&self.used.to_le_bytes());
        let end = Self::SIZE;
        buf[end - 4..end].copy_from_slice(&[0, 0, 0, 0]);
        let crc = crc32_skipping(buf);
        buf[end - 4..end].copy_from_slice(&crc.to_le_bytes());
    }

    /// Decode and validate a header from raw bytes. Fails on CRC mismatch or
    /// unknown future format version.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        debug_assert!(buf.len() >= Self::SIZE);
        let end = Self::SIZE;
        let stored = u32::from_le_bytes(buf[end - 4..end].try_into().unwrap());
        if stored != crc32_skipping(buf) {
            return Err(Error::ChecksumMismatch {
                page: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
            });
        }
        let page = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        let format_version = u16::from_le_bytes(buf[8..10].try_into().unwrap());
        if format_version > FORMAT_VERSION {
            return Err(Error::Other(format!(
                "page format v{format_version} newer than engine v{FORMAT_VERSION}"
            )));
        }
        Ok(Self {
            checksum: stored,
            page_id: page,
            format_version,
            flags: u16::from_le_bytes(buf[10..12].try_into().unwrap()),
            used: u16::from_le_bytes(buf[12..14].try_into().unwrap()),
        })
    }
}

/// Storage format version. Bump policy documented before M1 ships (D-003).
pub const FORMAT_VERSION: u16 = 1;

/// CRC32 (IEEE) over everything except the checksum slot itself
/// (`[14..18)`), so payload corruption is caught on load. Placeholder until
/// M2 swaps in a hardware-accelerated streaming impl (crc32fast / PCLMULQDQ);
/// signature stays identical.
pub(crate) fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Whole-buffer CRC that skips the header checksum slot.
fn crc32_skipping(buf: &[u8]) -> u32 {
    const SKIP: std::ops::Range<usize> = PageHeader::OFF_CHECKSUM..PageHeader::SIZE;
    if buf.len() <= SKIP.end {
        return crc32(&buf[..SKIP.start]);
    }
    let mut crc: u32 = 0xFFFF_FFFF;
    let parts: [&[u8]; 2] = [&buf[..SKIP.start], &buf[SKIP.end..]];
    for chunk in parts {
        for &b in chunk {
            crc ^= b as u32;
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let mut buf = [0u8; PAGE_SIZE];
        let hdr = PageHeader::new(42);
        hdr.encode_into(&mut buf);
        let decoded = PageHeader::decode(&buf).unwrap();
        // checksum is materialized during encode; compare semantic fields
        assert_eq!(decoded.page_id, hdr.page_id);
        assert_eq!(decoded.format_version, FORMAT_VERSION);
        assert_eq!(decoded.flags, hdr.flags);
        assert_eq!(decoded.used, hdr.used);
        assert_ne!(decoded.checksum, 0, "encode must seal a real checksum");
    }

    #[test]
    fn corrupted_header_is_rejected() {
        let mut buf = [0u8; PAGE_SIZE];
        PageHeader::new(7).encode_into(&mut buf);
        buf[3] ^= 0xFF; // inside CRC-covered region [0..Self::SIZE-4]
        assert!(matches!(
            PageHeader::decode(&buf),
            Err(Error::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn corrupted_payload_is_rejected() {
        // Full-page checksum contract: silent payload corruption must fail
        // validation at load time, not surface as garbage rows later.
        let mut buf = [0u8; PAGE_SIZE];
        PageHeader::new(8).encode_into(&mut buf);
        buf[4096] ^= 0xFF;
        assert!(matches!(
            PageHeader::decode(&buf),
            Err(Error::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn future_version_rejected() {
        let mut buf = [0u8; PAGE_SIZE];
        let mut hdr = PageHeader::new(1);
        hdr.format_version = FORMAT_VERSION + 1;
        hdr.encode_into(&mut buf);
        assert!(PageHeader::decode(&buf).is_err());
    }
}
