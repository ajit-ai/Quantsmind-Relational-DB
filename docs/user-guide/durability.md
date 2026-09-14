# Durability (User Guide, R2)

> R2 deliverable. What "committed" means and what survives a crash, for
> SQL users of the engine.

## What "committed" means

Every SQL statement (INSERT, CREATE TABLE, CREATE INDEX, DROP INDEX) is
committed individually. When the statement returns successfully, its data is
on disk and will survive a crash. There is no explicit BEGIN/COMMIT
yet -- every statement is its own transaction.

## What survives a crash

After any process crash (kill -9, power loss, panic), reopening the database
yields exactly the state that was committed before the crash:

- All committed INSERTs are present.
- All committed DDL (CREATE TABLE/INDEX, DROP INDEX) is present.
- Statements that were in progress when the crash happened are not visible.

There is no scenario where a committed statement is lost, and no scenario
where an incomplete statement becomes visible.

## What about the WAL?

The WAL is an internal detail of how durability works. You do not need to
manage it. The server or embed layer opens and manages the WAL file
automatically. Do not delete or modify `wal/wal.log` manually; doing so
may corrupt the database.

## Close vs crash

Calling `close()` or `drop()` on the engine flushes pending writes for
hygiene, but durability does not depend on it. A clean shutdown and a
hard crash result in the same committed state on restart.