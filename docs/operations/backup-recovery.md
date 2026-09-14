# Backup and Recovery -- Operation Guide (R2)

> R2 deliverable. Step-by-step operational commands for backup and restore.

## Back up a database

```sh
# 1. Stop the server first (no active writes).
#    (Mac/Linux) kill -TERM <pid> ; (Windows) stop the process

# 2. Copy the whole database directory.
cp -r /var/lib/qminddb /backup/qminddb-20260914
# or on Windows:
# robocopy C:\data\qminddb D:\backup\qminddb /E
```

The directory must contain `db.meta` (28 bytes) and `wal/wal.log`. Verify:

```sh
ls -la /backup/qminddb-20260914/db.meta
# -rw-r--r--  1 user user  28 ...
```

## Restore a database

```sh
# 1. Stop the server.
# 2. Replace the data directory with the backup.
rm -rf /var/lib/qminddb
cp -r /backup/qminddb-20260914 /var/lib/qminddb

# 3. Start the server; it replays the WAL automatically.
```

## Version compatibility

`db.meta` records five version fields (meta, db, format, wal, catalog), all
currently `1`. A backup made by a build with a newer format version will be
rejected with `Error::UnsupportedFormat` by an older build. Keep builds and
backups on the same format version. Before upgrading a database directory,
take a backup first: format changes are not yet auto-migrated.

## Tuning note

Write throughput in R2 is bounded by the per-statement fsync. There is no
group-commit batching yet (R3+). If you see slow inserts, they are not a
bug -- they are the durability guarantee doing its job.