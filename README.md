# mySSH

Windows 桌面 SSH 客户端，对标 Xshell / FinalShell。本地优先、无需登录、无云端依赖。

Tauri 2（Rust 后端 + WebView2）+ React 前端。

## 功能

- **终端**：多标签 + 任意方向分屏、标签拖拽重排/分离为独立窗口、断线自动重连、真彩色 / Unicode 宽字符 / 鼠标上报 / 括号粘贴 / 搜索 / 超链接；终端间输入广播（可选按服务器过滤）
- **会话管理**：分组树（嵌套分组、拖拽移动）、标签、收藏、模糊搜索、命令面板（Ctrl+Shift+P）；密码 / 公钥（OpenSSH + PuTTY .ppk）/ keyboard-interactive（2FA）/ agent 认证；跳板机多级 ProxyJump；known_hosts 首次连接与密钥变更弹窗确认
- **凭据安全**：Windows DPAPI 加密保险库，凭据永不明文落盘、不出现在日志与导出明文包中
- **隧道**：本地 / 远程 / 动态 SOCKS5 端口转发，开机自启、随会话自动建立、断线自动恢复；独立的 SSH 连接（与交互终端传输层隔离，大流量不卡终端）
- **SFTP**：左右分栏文件管理器，本地/远程双向拖拽、OS 文件直接拖入上传（含文件夹递归）；队列化传输（并发控制、断点续传、失败重试）、跨会话传输历史；与终端当前目录联动（OSC 7）；远程文件直编（本地编辑器打开、保存自动回传）
- **监控**：CPU / 内存 / 磁盘 / 网络实时图表（独立采样通道，采集失败静默降级）
- **导入导出**：OpenSSH config / PuTTY 注册表 / Xshell / FinalShell 会话导入；配置导出（明文或 Argon2id+AES-256-GCM 口令加密）
- **主题**：多套配色（暗色/浅色/Nord 等），UI 与终端配色独立

## 技术栈

| 层 | 选型 |
|---|---|
| 桌面框架 | Tauri 2.11（Rust + WebView2） |
| 前端 | TypeScript + React 19 + Vite + zustand + Tailwind 4 |
| 终端渲染 | xterm.js 6 + WebGL addon（canvas 降级） |
| SSH | russh 0.62 / russh-sftp 2.4 |
| 存储 | SQLite（sqlx）+ DPAPI 凭据保险库 |
| 异步 | tokio |

## 构建

依赖：Rust stable、Node.js 20+、Windows 10/11（WebView2 Runtime，Win11 预装）。

```bash
# 前端
cd app/ui
npm install
npm run build

# 桌面应用（release 可执行文件）
cargo build -p app --release
# 产物：target/release/app.exe

# 安装包（NSIS + MSI）
app/ui/node_modules/.bin/tauri.cmd build   # 在仓库根执行
# 产物：target/release/bundle/
```

开发模式（热重载）：

```bash
cd app/ui && npm run dev        # Vite dev server
cargo run -p app                # 另开终端
```

## 测试与质量

```bash
# Rust（仓库根）
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# 前端（app/ui 下）
npm run lint
npm test
```

工程约束：非测试代码禁止 `unwrap`/`expect`（例外须附 `// SAFETY:` 注释）、禁止 `unsafe`；终端数据走 Tauri Channel 二进制流式推送（非 JSON 事件）；隧道/SFTP 使用独立 SSH 连接，与交互终端传输层隔离。

## 目录结构

```
crates/
  app/          Tauri 壳：命令层、IPC 装配
  core-ssh/     SSH 协议核心（连接、认证、通道）
  core-tunnel/  端口转发
  core-sftp/    SFTP 与传输队列
  core-monitor/ 服务器指标采集
  core-store/   SQLite 持久化、凭据保险库、导入导出
  core-policy/  策略
  cli/          Agent CLI（规划中）
app/ui/         前端（React + xterm）
```

## 数据位置

`%LOCALAPPDATA%\myssh\`：`myssh.db`（SQLite，会话/隧道/传输历史等元数据）、凭据（DPAPI 加密，绑定当前 Windows 用户与机器）、`known_hosts`、`logs/`。

## 路线图

- MCP Server / CLI：作为 AI agent 操作服务器的受控执行层（规划中）

## 许可证

[Apache License 2.0](LICENSE)
