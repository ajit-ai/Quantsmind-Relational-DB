# Backup and Recovery Model (R2)

> R2 deliverable. Operational guidance for backing up and recovering a
> QuantsMind database.

## Backup

The database directory is a single folder containing:
- `db.meta` -- 28-byte metadata header (must be present and valid)
- `wal/wal.log` -- the WAL file containing all committed and in-flight data
- Reserved subdirectories (`catalog/`, `tables/`, `indexes/`, etc.)

A **consistent backup** is the entire directory copied atomically while the
database is not running (or while the process has `close()`d cleanly).

**Online backup** (while the database is running) is not yet supported.
R3 on-disk structures may allow snapshot-based online backups; this is out
of scope for R2.

### Backup procedure (R2)

1. Stop the server or drop the engine's write lock (no active inserts).
2. Copy the entire database directory to the backup location.
3. Verify `db.meta` exists in the backup and is 28 bytes.

### Restore procedure

1. Stop any running server.
2. Replace the database directory with the backup copy.
3. Restart the server; `open_db` replays the WAL and rebuilds state.

## Recovery guarantees

| Scenario | What happens |
|----------|-------------|
| Process crash during a committed statement | Statement is present after restart (was fsynced before returning Ok) |
| Process crash during a statement (before commit) | Statement is not visible after restart (Commit frame absent from WAL) |
| Mid-write power loss (torn frame) | Torn tail is truncated on open; committed prefix intact |
| Interior corruption (bit-flip, disk error) | `open_db` fails loudly with `WalCorrupt` |
| Version mismatch (old format, foreign file) | `open_db` fails with `UnsupportedFormat` |

**No data loss of committed transactions** is the R2 contract. This is
proven by 7 subprocess crash-recovery tests (`crash_recovery.rs`) that
kill the process with `std::process::exit` (bypassing destructors) and
verify the database state on restart.

## Limitations (R2)

- No online backup support.
- No point-in-time recovery (PITR) -- R3+ after WAL segment rotation.
- No WAL archiving -- the single `wal.log` file is the entire history.
- No logical replication -- R7+ if decided.