# QuantsMind — One-Click Install Guide

## Windows

### Option 1: PowerShell (Recommended)
```powershell
irm https://raw.githubusercontent.com/ajit-ai/Quantsmind-Relational-DB/main/packaging/windows/install.ps1 | iex
```

### Option 2: Scoop
```powershell
scoop bucket add quantsmind https://github.com/ajit-ai/Quantsmind-Relational-DB
scoop install qmind
```

### Option 3: Download directly
1. Go to https://github.com/ajit-ai/Quantsmind-Relational-DB/releases
2. Download `qmind-windows-x64-0.1.0.zip`
3. Extract to `C:\Program Files\QuantsMind\`
4. Add `C:\Program Files\QuantsMind` to PATH

### Option 4: Desktop App (GUI)
Download `QuantsMind-Studio-windows-x64.exe` from Releases → run installer.

---

## Linux

### Option 1: One-Line Install (All Distros)
```bash
curl -sSL https://raw.githubusercontent.com/ajit-ai/Quantsmind-Relational-DB/main/packaging/linux/install.sh | bash
```
Auto-detects: Ubuntu, Debian, Fedora, RHEL, Arch, Alpine, openSUSE.

### Option 2: Ubuntu/Debian (.deb)
```bash
# Download from Releases
wget https://github.com/ajit-ai/Quantsmind-Relational-DB/releases/download/v0.1.0/qmind-linux-x64-0.1.0.tar.gz
tar xzf qmind-linux-x64-0.1.0.tar.gz
sudo install qmind-server qmind-cli /usr/local/bin/
```

### Option 3: Static Binary (Any Linux — No Dependencies)
```bash
wget https://github.com/ajit-ai/Quantsmind-Relational-DB/releases/download/v0.1.0/qmind-linux-x64-static-0.1.0.tar.gz
tar xzf qmind-linux-x64-static-0.1.0.tar.gz
sudo install qmind-server qmind-cli /usr/local/bin/
```

### Option 4: ARM64 (Raspberry Pi, AWS Graviton)
```bash
wget https://github.com/ajit-ai/Quantsmind-Relational-DB/releases/download/v0.1.0/qmind-linux-arm64-0.1.0.tar.gz
tar xzf qmind-linux-arm64-0.1.0.tar.gz
sudo install qmind-server qmind-cli /usr/local/bin/
```

### Option 5: Desktop App (GUI)
Download `QuantsMind-Studio-linux-x64.deb` or `.AppImage` from Releases.

### Uninstall
```bash
curl -sSL https://raw.githubusercontent.com/ajit-ai/Quantsmind-Relational-DB/main/packaging/linux/uninstall.sh | bash
```

---

## macOS

### Option 1: One-Line Install
```bash
curl -sSL https://raw.githubusercontent.com/ajit-ai/Quantsmind-Relational-DB/main/packaging/macos/install.sh | bash
```
Auto-detects Apple Silicon (M1/M2/M3) vs Intel.

### Option 2: Homebrew
```bash
brew tap ajit-ai/quantsmind
brew install quantsmind
```

### Option 3: Download directly
1. Go to https://github.com/ajit-ai/Quantsmind-Relational-DB/releases
2. Download `qmind-macos-arm64-0.1.0.tar.gz` (Apple Silicon) or `qmind-macos-x64-0.1.0.tar.gz` (Intel)
3. Extract and install:
```bash
tar xzf qmind-macos-*.tar.gz
sudo install qmind-server qmind-cli /usr/local/bin/
```

### Option 4: Desktop App (GUI)
Download `QuantsMind-Studio-macos.dmg` from Releases → drag to Applications.

### Uninstall
```bash
curl -sSL https://raw.githubusercontent.com/ajit-ai/Quantsmind-Relational-DB/main/packaging/macos/uninstall.sh | bash
```

---

## FreeBSD

```bash
fetch -qO- https://raw.githubusercontent.com/ajit-ai/Quantsmind-Relational-DB/main/packaging/linux/freebsd-install.sh | bash
```

### Manual
```bash
fetch https://github.com/ajit-ai/Quantsmind-Relational-DB/releases/download/v0.1.0/qmind-freebsd-x64-0.1.0.tar.gz
tar xzf qmind-freebsd-x64-0.1.0.tar.gz
sudo install qmind-server qmind-cli /usr/local/bin/
```

### As a Service
```bash
sysrc qmind_enable=YES
service qmind start
```

---

## Build from Source (Any Platform)

### Prerequisites
- Rust 1.75+ (https://rustup.rs)
- Git

### Quick Build
```bash
git clone https://github.com/ajit-ai/Quantsmind-Relational-DB.git
cd Quantsmind-Relational-DB
cargo build --release
```

Binaries at: `target/release/qmind-server`, `target/release/qmind-cli`

### Cross-Compile for All Platforms
```bash
# From Linux: build all targets
./scripts/build-all.sh v0.1.0

# From Windows: build all targets
.\scripts\build-all.ps1 -Tag v0.1.0
```

---

## After Install — Quick Start

### Start the Server
```bash
qmind-server 5432
```

### Connect with CLI
```bash
qmind-cli 127.0.0.1:5432
```

### Try SQL
```sql
CREATE TABLE users (id INTEGER, name TEXT, dept TEXT);
INSERT INTO users VALUES (1, 'alice', 'engineering');
INSERT INTO users VALUES (2, 'bob', 'sales');
SELECT * FROM users WHERE id = 1;
SELECT dept, COUNT(*), AVG(id) FROM users GROUP BY dept;
```

### Connect with psql (Postgres compatible)
```bash
psql -h 127.0.0.1 -p 5432 -U nobody
```

---

## Run as System Service

### Linux (systemd)
```bash
sudo systemctl start qmind
sudo systemctl enable qmind    # auto-start on boot
```

### macOS (launchd)
```bash
launchctl load ~/Library/LaunchAgents/com.quantsmind.server.plist
launchctl start com.quantsmind.server
```

### Windows
```powershell
Start-Service QuantsMind
Set-Service -Name QuantsMind -StartupType Automatic
```

### FreeBSD
```bash
service qmind start
sysrc qmind_enable=YES
```

---

## Verify Installation

```bash
# Check version
qmind-server --version

# Run tests (from source)
cargo test --workspace

# Check benchmarks
cargo bench -p qmind-kernel
```

---

## Troubleshooting

| Problem | Fix |
|---|---|
| `qmind-server: command not found` | Add install dir to PATH or use full path |
| `port 5432 already in use` | Use different port: `qmind-server 5433` |
| Permission denied | Use `sudo` for system-wide install |
| macOS Gatekeeper blocks | `xattr -d com.apple.quarantine /usr/local/bin/qmind-server` |
| Linux missing libssl | Install: `apt install libssl-dev` or use static binary |
