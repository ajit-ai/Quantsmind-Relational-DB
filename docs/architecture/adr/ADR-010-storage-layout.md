# ADR-010 -- Storage Layout

- **Status**: Accepted (R2)
- **Date**: R2 architecture gate

## Context

The engine needs a canonical on-disk structure that distinguishes a fresh
run from a reuse run, carries format version information, and reserves
space for future on-disk structures (row store, indexes, columnar,
checkpoints) without forcing a migration when those land.

## Decision

The database is a directory with a fixed internal layout:

```
<root>/
  db.meta          28-byte versioned header (magic + 5 version fields)
  wal/wal.log      append-only WAL
  catalog/         reserved (R3)
  tables/          reserved (R3)
  indexes/         reserved (R3)
  columnar/        reserved (R3)
  checkpoints/     reserved (R3)
```

`db.meta` is the unique identifier: its presence means a QuantsMind
database; its magic and version fields gate the engine. All version fields
are currently `1`; any mismatch returns `Error::UnsupportedFormat`.

`create_db` creates the full directory structure including the reserved
subdirectories. `open_db` requires `db.meta` to exist; a missing file
is `Error::CatalogCorrupt` (not-found is not retried as a fresh-create).

## Consequences

- Simple, inspectable layout (no hidden dotfiles).
- Version gates are enforced at open time before any WAL replay; a bad
  version is rejected immediately.
- Reserved directories exist from the first create, so R3 on-disk work
  can land without a migration step.

## References

- `qmind-sql/src/dbdir.rs` (`create_layout`, `write_meta`, `read_meta`)
- `STORAGE_LAYOUT.md`