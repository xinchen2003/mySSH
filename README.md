# mySSH

English | [简体中文](README.zh-CN.md)

A local-first SSH client for Windows. No login, no cloud dependency.

Built with Tauri 2 (Rust backend + WebView2) and a React frontend.

## Features

- **Terminal**: tabs, split panes in any direction, drag tabs out into separate windows, auto-reconnect, true color / wide Unicode / mouse reporting / bracketed paste / search / hyperlinks; optional input broadcasting across terminals (filterable by server)
- **Sessions**: nested group tree (drag & drop), tags, favorites, fuzzy search, command palette (Ctrl+Shift+P); password / public-key (OpenSSH & .ppk) / keyboard-interactive (2FA) / agent auth; multi-hop ProxyJump; known_hosts confirmation on first connect and key change
- **Credential safety**: Windows DPAPI vault — credentials never hit disk in plaintext, never appear in logs or plaintext exports
- **Tunnels**: local / remote / dynamic SOCKS5 forwarding, auto-start, auto-recover on disconnect; runs on a dedicated SSH connection isolated from interactive terminals
- **SFTP**: dual-pane file manager, bidirectional drag & drop (including OS files and folders), queued transfers (concurrency control, resume, retry), cross-session transfer history, remote file editing (open in local editor, auto-upload on save), follows the terminal's working directory (OSC 7)
- **Monitoring**: live CPU / memory / disk / network charts on an isolated channel, silently degrades on failure
- **Import / Export**: import sessions from common SSH client formats; export config in plaintext or passphrase-encrypted (Argon2id + AES-256-GCM)
- **Themes**: multiple color schemes (dark / light / Nord and more), UI and terminal palettes independent

## Tech Stack

| Layer | Choice |
|---|---|
| Desktop | Tauri 2.11 (Rust + WebView2) |
| Frontend | TypeScript + React 19 + Vite + zustand + Tailwind 4 |
| Terminal | xterm.js 6 + WebGL addon (canvas fallback) |
| SSH | russh 0.62 / russh-sftp 2.4 |
| Storage | SQLite (sqlx) + DPAPI credential vault |
| Async | tokio |

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

## Roadmap

- MCP Server / CLI: a controlled execution layer for AI agents to operate servers

## License

[Apache License 2.0](LICENSE)
