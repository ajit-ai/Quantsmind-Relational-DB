# Workload: Recovery

- **Workload**: `recovery` — Crash / restart / checkpoint behavior.
- **Primary scale**: S1–S4.

## SQL shape

Not a SQL query workload. Recovery behavior is measured at the server/CLI
level:

1. `INSERT` N rows (document batch size).
2. Send SIGKILL (Linux) or taskkill /F (Windows) — no graceful shutdown.
3. Restart the engine binary and open the same WAL/directory.
4. Verify committed rows are present; record restart time and WAL replay time.

## Measurement contract

### Procedure

1. Start server/embed on an empty database at the target scale.
2. Insert a known number of rows (N); record exact N.
3. Kill the process (non-graceful).
4. Restart; wait for ready state (open + WAL replay complete).
5. Count rows; compute recovery time and commit continuity.

### Metrics

| Metric | Unit |
|---|---|
| rows committed before kill | rows |
| rows present after restart | rows (must == pre-kill) |
| WAL bytes generated | bytes |
| restart/replay wall time | ms |
| checkpoint was triggered before kill (R2+) | bool |
| WAL truncation observed (R2+) | bool |

### Environment (record per run)

- commit hash, OS, CPU, RAM, disk (latency class matters), build mode.

### Notes

- Document any corruption detection (CRC failure) behavior separately; it is
  a valid valuable result, not a failure of the workload.
- If a stage does not support durable WAL (R1), document that the test
  intentionally demonstrates data loss — that is the honest R1 finding.

## Acceptance linkage

`BILLION_ROW_REQUIREMENTS.md` §5 (reliability: crash recovery, checkpoint,
WAL, restart, corruption detection).