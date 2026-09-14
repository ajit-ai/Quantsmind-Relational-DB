# Results

> `benchmarks/results/` — **real, reproducible benchmark results only.**

R1 does NOT generate results. This directory exists to define where results
land and to make their provenance explicit.

## Structure

```text
results/
├── S1/   1M-row reports
├── S2/   10M-row reports
├── S3/   100M-row reports
└── S4/   1B-row reports
```

Each report file names the workload it is for, e.g.
`S3/full_scan.md` or `S4/recovery.md`.

## Report template (mandatory for any committed result)

```markdown
# <workload> @ <scale>

- commit: <git hash>
- date: <date>
- env: <cpu>/<ram>/<disk>; os
- build: cargo build --release (profile.bench not asserted here)
- generator: <datasets manifest id>

## Procedure
- <exact steps, honoring the workload spec>

## Metrics
| metric | value | unit |

## Notes
- <deviations, anomalies>

## Verdict vs BILLION_ROW_REQUIREMENTS acceptance
- <pass/fail + evidence>
```

## Rules

1. No fabricated numbers, ever (R1 engineering rule 11).
2. Every result is reproducible from the listed procedure + commit.
3. If a workload cannot pass at a scale, record it as a **fail with evidence** —
   that is still a valid, valuable result.