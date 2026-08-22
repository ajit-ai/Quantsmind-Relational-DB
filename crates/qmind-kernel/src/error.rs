use crate::wal::Lsn;
use crate::PageId;
use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    /// Page header failed CRC validation — on-disk corruption detected.
    ChecksumMismatch {
        page: PageId,
    },
    /// Requested page does not exist in the store.
    PageNotFound {
        page: PageId,
    },
    /// WAL is missing records required for recovery.
    WalGap {
        at: Lsn,
    },
    /// WAL record failed checksum or framing validation during replay.
    WalCorrupt {
        at: Lsn,
        reason: String,
    },
    /// Every frame is pinned or referenced — pool capacity exhausted.
    NoEvictableFrame,
    Io(std::io::Error),
    Other(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::ChecksumMismatch { page } => write!(f, "checksum mismatch on page {page}"),
            Error::PageNotFound { page } => write!(f, "page {page} not found"),
            Error::WalGap { at } => write!(f, "gap in WAL at LSN {at}"),
            Error::WalCorrupt { at, reason } => {
                write!(f, "corrupt WAL record at LSN {at}: {reason}")
            }
            Error::NoEvictableFrame => write!(f, "buffer pool exhausted: all frames pinned"),
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
