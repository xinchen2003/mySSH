# mySSH

English | [简体中文](README.zh-CN.md)

[![CI](https://github.com/xinchen2003/mySSH/actions/workflows/ci.yml/badge.svg)](https://github.com/xinchen2003/mySSH/actions/workflows/ci.yml)
[![Version](https://img.shields.io/badge/version-0.3.2-blue)](https://github.com/xinchen2003/mySSH/releases)
[![License](https://img.shields.io/badge/license-MIT-green)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11-lightgrey)](https://github.com/xinchen2003/mySSH)

**A local-first SSH client for Windows with a built-in MCP server — hand your servers to AI agents, on your terms.**

No login, no cloud dependency, credentials never leave your machine. Built with Tauri 2 (Rust + WebView2) and React.

## Why mySSH

- **AI-ready**: a built-in MCP server lets Claude Code / OMP / OpenCode run commands, read/write and transfer files on your saved sessions — with per-session × per-tool-group permissions you control
- **Local-first**: config in local SQLite, credentials in the Windows DPAPI vault, never on disk in plaintext
- **Performance budgeted**: terminal IPC streams over a binary Tauri Channel (not JSON events); tunnels / SFTP / monitoring each run on dedicated SSH connections, so bulk traffic never stutters your interactive shell

## Features

- **Terminal**: tabs, split panes in any direction, drag tabs out into separate windows, auto-reconnect; true color / wide Unicode / mouse reporting / bracketed paste / search / hyperlinks; optional input broadcasting across terminals (filterable by server); in-terminal file transfer via ZMODEM (rz/sz) and trzsz
- **Sessions**: nested group tree, move-to-group context menu, tags, favorites, fuzzy search, command palette (Ctrl+Shift+P); password / public-key (OpenSSH & .ppk) / keyboard-interactive (2FA) / agent auth; multi-hop ProxyJump; known_hosts confirmation on first connect and key change; login macros — run commands line-by-line automatically after reaching the shell; optional su second-login — sign in as a regular user and auto-switch to another account (e.g. root), password answered once at the prompt
- **Local sessions**: native Windows shells (PowerShell / pwsh / CMD) via ConPTY alongside SSH sessions — custom startup directory and startup command (e.g. launch an AI agent CLI on connect); shares tabs, splits, detached windows and input broadcasting
- **Tunnels**: local / remote / dynamic SOCKS5 forwarding, auto-start, auto-recover on disconnect; dedicated SSH connection isolated from interactive terminals
- **SFTP**: dual-pane file manager, bidirectional drag & drop (including OS files and folders), queued transfers (concurrency control, resume, retry), cross-session transfer history, remote file editing (auto-upload on save), follows the terminal's working directory (OSC 7)
- **Monitoring**: live CPU / memory / disk / network charts on an isolated channel, silently degrades on failure
- **Export**: plaintext or passphrase-encrypted (Argon2id + AES-256-GCM) config packages
- **Themes**: 8 built-in schemes (One Dark / Solarized / Nord / Midnight / GitHub / Eye-care Green / Warm), visual custom theme editor (no JSON hand-editing), custom terminal background image with adjustable opacity
- **i18n**: Simplified Chinese / English UI, switchable in Settings
- **Terminal encoding**: per-session remote encoding (UTF-8 / GBK / GB18030 / Big5 / Shift_JIS / EUC-KR), streaming transcode with zero-copy passthrough for UTF-8

## AI Automation (MCP)

Built-in MCP server (Streamable HTTP, loopback-only + Bearer token); one-click agent config copy in Settings.

**14 tools**:

| Tool | Description |
| --- | --- |
| `list_sessions` | List saved session profiles (never includes credentials) |
| `ssh_exec` | Run a shell command on a session, returns stdout/stderr/exit code |
| `sftp_home` / `sftp_list` / `sftp_stat` / `sftp_read` | Remote browsing and file reads |
| `sftp_write` / `sftp_mkdir` / `sftp_delete` / `sftp_rename` / `sftp_chmod` | Remote writes and meta ops (audited) |
| `sftp_upload` / `sftp_download` | File transfer via the background transfer queue (shared with the UI transfer panel, **no size limit**; `wait_seconds` waits synchronously for completion) |
| `sftp_transfer_list` | Poll transfer progress / state / errors |

**Permission model** (two layers, override wins):

1. Global defaults: Settings → MCP → Tool permissions — 5 group toggles (list_sessions / ssh_exec / SFTP read / SFTP write / SFTP transfer)
2. Per-session overrides: session editor → MCP Permissions tab — each group is tri-state (inherit / allow / deny). Lock down `ssh_exec` on production, keep test boxes wide open

Config example (`.omp/mcp.json`, same shape for Claude Code):

```json
{
  "mcpServers": {
    "myssh": {
      "type": "http",
      "url": "http://127.0.0.1:17345/mcp",
      "headers": { "Authorization": "Bearer <token>" }
    }
  }
}
```

## Roadmap

- Agent CLI: scriptable command-line interface (planned)

## Tech Stack

| Layer       | Choice                                              |
| ----------- | --------------------------------------------------- |
| Desktop     | Tauri 2.11 (Rust + WebView2)                        |
| Frontend    | TypeScript + React 19 + Vite + zustand + Tailwind 4 |
| Terminal    | xterm.js 6 + WebGL addon (canvas fallback)          |
| SSH         | russh 0.62 / russh-sftp 2.4                         |
| Local shell | portable-pty (ConPTY)                               |
| Storage     | SQLite (sqlx) + DPAPI credential vault              |
| Async       | tokio                                               |

## Build

Requirements: Rust stable, Node.js 20+, Windows 10/11 (WebView2 Runtime).

```bash
# Frontend
cd app/ui
npm install
npm run build

# Desktop app (release binary)
cargo build -p app --release
# Output: target/release/app.exe

# Installers (NSIS + MSI) — run from repo root
app/ui/node_modules/.bin/tauri.cmd build
# Output: target/release/bundle/
```

Dev mode with hot reload:

```bash
cd app/ui && npm run dev        # Vite dev server
cargo run -p app                # in another terminal
```

## Tests & Quality

```bash
# Rust (repo root)
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Frontend (app/ui)
npm run lint
npm test
```

Engineering rules: no `unwrap`/`expect` outside tests (exceptions need a `// SAFETY:` comment), no `unsafe`; terminal output streams over a Tauri IPC Channel (binary, not JSON events); tunnels/SFTP run on dedicated SSH connections isolated from interactive terminals.

## Project Layout

```
crates/
  app/          Tauri shell: command layer, IPC wiring
  core-ssh/     SSH protocol core (connections, auth, channels)
  core-tunnel/  Port forwarding
  core-sftp/    SFTP & transfer queue
  core-monitor/ Server metrics
  core-store/   SQLite persistence, credential vault, import/export
  core-policy/  Policy
  cli/          Agent CLI (planned)
app/ui/         Frontend (React + xterm)
```

## Data Location

`%LOCALAPPDATA%\myssh\`: `myssh.db` (SQLite: sessions, tunnels, transfer history), credentials (DPAPI-encrypted, bound to the current Windows user and machine), `known_hosts`, `logs/`.

## License

[MIT License](LICENSE)
