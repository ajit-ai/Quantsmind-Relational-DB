# Reproducible R3 benchmark run — Windows PowerShell
#
# Records the environment block (commit, rustc, CPU, RAM) into R3_BENCH_*
# env vars consumed by the harness, then runs the release binary in a
# scratch target dir and leaves the report in benchmarks/results/r3/.

param(
    [int]$Rows = 100000,
    [int]$Iterations = 3,
    [string]$Out = "benchmarks/results/r3"
)

$ErrorActionPreference = "Stop"

$env:R3_BENCH_COMMIT = (& git rev-parse HEAD 2>$null)
if (-not $env:R3_BENCH_COMMIT) { $env:R3_BENCH_COMMIT = "n/a (not a git checkout)" }

$rustc = & rustc --version 2>$null
$env:R3_BENCH_RUSTC = if ($rustc) { "$rustc" } else { "n/a" }

$env:R3_BENCH_CPU = if ($env:PROCESSOR_IDENTIFIER) { $env:PROCESSOR_IDENTIFIER } else { "n/a" }

$ram = (Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory
$env:R3_BENCH_RAM = if ($ram) { "{0} bytes" -f $ram } else { "n/a" }

$env:CARGO_TARGET_DIR = "target-repro"

Write-Host "commit=$env:R3_BENCH_COMMIT rustc=$env:R3_BENCH_RUSTC cpu=$env:R3_BENCH_CPU ram=$env:R3_BENCH_RAM"
cargo run -p qmind-sql --release --bin bench_r3 -- `
    --rows $Rows --iterations $Iterations --out $Out

if ($LASTEXITCODE -ne 0) {
    Write-Error "bench_r3 exited with code $LASTEXITCODE"
}