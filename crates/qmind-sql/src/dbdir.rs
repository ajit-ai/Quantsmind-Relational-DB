//! Durable database directory layout and versioned metadata (R2.6, R2.7, R2.16).
//!
//! Layout of a database root:
//!
//! ```text
//! <root>/
//!   db.meta            versioned metadata (magic + 4 format versions)
//!   wal/wal.log        the write-ahead log (durable record of everything)
//!   catalog/           RESERVED — durable catalog files land here in later
//!                      milestones; in R2 the catalog lives in the WAL
//!   tables/            RESERVED — page/heap row storage lands here (R3)
//!   indexes/           RESERVED — persistent index structures (R3+)
//!   columnar/          RESERVED — persistent columnar segments (M8)
//!   checkpoints/       RESERVED — checkpoint state (R2.11 interface)
//! ```
//!
//! Opening a database validates all four version numbers against the
//! building binary; an unsupported version fails loudly with
//! [`Error::UnsupportedFormat`] instead of risking silent corruption.

use qmind_kernel::{Error, Result};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Fixed magic prefix of `db.meta`.
pub const META_MAGIC: &[u8; 8] = b"QMDBMETA";
/// Version of the `db.meta` encoding itself.
pub const META_VERSION: u32 = 1;
/// Application database version.
pub const DB_VERSION: u32 = 1;
/// On-disk storage format version.
pub const FORMAT_VERSION: u32 = 1;
/// WAL frame/record format version.
pub const WAL_VERSION: u32 = 1;
/// Catalog record format version.
pub const CATALOG_VERSION: u32 = 1;

/// Canonical subdirectory names. `catalog/`, `tables/`, `indexes/`,
/// `columnar/` and `checkpoints/` are created to keep the layout stable but
/// their contents are owned by later milestones.
pub const WAL_DIR: &str = "wal";
pub const CATALOG_DIR: &str = "catalog";
pub const TABLES_DIR: &str = "tables";
pub const INDEXES_DIR: &str = "indexes";
pub const COLUMNAR_DIR: &str = "columnar";
pub const CHECKPOINTS_DIR: &str = "checkpoints";

pub const META_FILE: &str = "db.meta";
pub const WAL_FILE: &str = "wal.log";

/// Versioned metadata persisted in `db.meta`.
///
/// Layout: `[QMDBMETA][meta u32][db u32][format u32][wal u32][catalog u32]`
/// (8 + 5 x 4 = 28 bytes, little-endian).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DbMeta {
    pub db_version: u32,
    pub format_version: u32,
    pub wal_version: u32,
    pub catalog_version: u32,
}

impl DbMeta {
    pub fn current() -> Self {
        Self {
            db_version: DB_VERSION,
            format_version: FORMAT_VERSION,
            wal_version: WAL_VERSION,
            catalog_version: CATALOG_VERSION,
        }
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(28);
        out.extend_from_slice(META_MAGIC);
        out.extend_from_slice(&META_VERSION.to_le_bytes());
        out.extend_from_slice(&self.db_version.to_le_bytes());
        out.extend_from_slice(&self.format_version.to_le_bytes());
        out.extend_from_slice(&self.wal_version.to_le_bytes());
        out.extend_from_slice(&self.catalog_version.to_le_bytes());
        out
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 28 || &bytes[..8] != META_MAGIC {
            return Err(Error::CatalogCorrupt {
                table: "<database>".into(),
                reason: "db.meta magic mismatch (not a QuantsMind database)".into(),
            });
        }
        let u32_at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
        let meta_version = u32_at(8);
        if meta_version != META_VERSION {
            return Err(Error::UnsupportedFormat {
                entity: "db.meta",
                found: meta_version,
                expected: META_VERSION,
            });
        }
        Ok(Self {
            db_version: u32_at(12),
            format_version: u32_at(16),
            wal_version: u32_at(20),
            catalog_version: u32_at(24),
        })
    }
}

fn validate_one(entity: &'static str, found: u32, expected: u32) -> Result<()> {
    if found != expected {
        return Err(Error::UnsupportedFormat {
            entity,
            found,
            expected,
        });
    }
    Ok(())
}

/// Reject metadata written by an incompatible engine version.
pub fn validate(meta: &DbMeta) -> Result<()> {
    let cur = DbMeta::current();
    validate_one("database", meta.db_version, cur.db_version)?;
    validate_one("storage format", meta.format_version, cur.format_version)?;
    validate_one("WAL", meta.wal_version, cur.wal_version)?;
    validate_one("catalog", meta.catalog_version, cur.catalog_version)?;
    Ok(())
}

pub fn meta_path(root: &Path) -> PathBuf {
    root.join(META_FILE)
}

pub fn wal_dir(root: &Path) -> PathBuf {
    root.join(WAL_DIR)
}

pub fn wal_path(root: &Path) -> PathBuf {
    wal_dir(root).join(WAL_FILE)
}

/// Create the canonical directory skeleton under `root` (idempotent).
pub fn create_layout(root: &Path) -> Result<()> {
    for dir in [
        WAL_DIR,
        CATALOG_DIR,
        TABLES_DIR,
        INDEXES_DIR,
        COLUMNAR_DIR,
        CHECKPOINTS_DIR,
    ] {
        fs::create_dir_all(root.join(dir))?;
    }
    Ok(())
}

/// Write the initial `db.meta` and sync the metadata durability boundary.
pub fn write_meta(root: &Path) -> Result<()> {
    let path = meta_path(root);
    let mut f = fs::File::create(&path)?;
    f.write_all(&DbMeta::current().encode())?;
    f.sync_all()?;
    Ok(())
}

/// Load and validate `db.meta`. Missing file or wrong magic is reported as
/// catalog corruption; an unsupported version as `UnsupportedFormat` —
/// opening never silently downgrades or guesses.
pub fn read_meta(root: &Path) -> Result<DbMeta> {
    let path = meta_path(root);
    if !path.exists() {
        return Err(Error::CatalogCorrupt {
            table: "<database>".into(),
            reason: format!(
                "missing db.meta (not a QuantsMind database) at {}",
                root.display()
            ),
        });
    }
    let mut bytes = Vec::new();
    fs::File::open(&path)?.read_to_end(&mut bytes)?;
    let meta = DbMeta::decode(&bytes)?;
    validate(&meta)?;
    Ok(meta)
}
