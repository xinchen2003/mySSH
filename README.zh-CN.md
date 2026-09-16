# mySSH

[English](README.md) | 简体中文

[![CI](https://github.com/xinchen2003/mySSH/actions/workflows/ci.yml/badge.svg)](https://github.com/xinchen2003/mySSH/actions/workflows/ci.yml)
[![Version](https://img.shields.io/badge/version-0.3.2-blue)](https://github.com/xinchen2003/mySSH/releases)
[![License](https://img.shields.io/badge/license-MIT-green)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11-lightgrey)](https://github.com/xinchen2003/mySSH)

**本地优先的 Windows SSH 客户端，内置 MCP 服务端——把服务器直接暴露给 AI agent 操作。**

无需登录、无云端依赖、凭据不出本机。Tauri 2（Rust + WebView2）+ React。

## 为什么是 mySSH

- **AI 就绪**：内置 MCP server，Claude Code / OMP / OpenCode 等 agent 可直接在你保存的会话上执行命令、读写文件、传文件——权限粒度到「会话 × 工具组」，你说了算
- **本地优先**：配置存本机 SQLite，凭据进 Windows DPAPI 保险库，永不明文落盘
- **性能有预算**：终端 IPC 走二进制 Channel 而非 JSON 事件；隧道 / SFTP / 监控各占独立 SSH 连接，大流量不卡交互终端

## 功能

- **终端**：多标签 + 任意方向分屏、标签拖拽分离为独立窗口、断线自动重连；真彩色 / Unicode 宽字符 / 鼠标上报 / 括号粘贴 / 搜索 / 超链接；终端间输入广播（可按服务器过滤）；ZMODEM（rz/sz）与 trzsz 终端内文件传输
- **会话管理**：嵌套分组树、右键移动到分组、标签、收藏、模糊搜索、命令面板（Ctrl+Shift+P）；密码 / 公钥（OpenSSH + .ppk）/ keyboard-interactive（2FA）/ agent 认证；多级跳板机；known_hosts 首连与密钥变更确认；登录宏（认证进 shell 后逐行自动执行命令）；su 二级登录（普通账号登入后自动切 root，密码存保险库、提示出现时自动应答一次）
- **本地会话**：ConPTY 承载 PowerShell / pwsh / CMD，与 SSH 会话并列；自定义启动目录与启动命令（如连接即拉起 AI agent CLI）
- **隧道**：本地 / 远程 / 动态 SOCKS5 转发，开机自启、随会话建立、断线自动恢复；独立 SSH 连接与交互终端隔离
- **SFTP**：双栏文件管理器、双向拖拽（含 OS 文件/文件夹直接拖入）、队列化传输（并发控制 / 断点续传 / 失败重试）、跨会话传输历史、远程文件直编（保存自动回传）、跟随终端当前目录（OSC 7）
- **监控**：CPU / 内存 / 磁盘 / 网络实时图表（独立采样通道，失败静默降级）
- **导出**：明文或口令加密（Argon2id + AES-256-GCM）配置包
- **主题与外观**：多套配色（暗色 / 浅色 / Nord 等）、自定义终端背景图与不透明度
- **多语言**：简体中文 / English，设置中切换
- **终端编码**：按会话配置（UTF-8 / GBK / GB18030 / Big5 / Shift_JIS / EUC-KR），流式转码，UTF-8 零拷贝直通

## AI 自动化（MCP）

内置 MCP 服务端（Streamable HTTP，仅监听 127.0.0.1 + Bearer 令牌），agent 配置在设置面板一键复制。

**14 个工具**：

| 工具 | 说明 |
| --- | --- |
| `list_sessions` | 列出会话档案（不含凭据） |
| `ssh_exec` | 在会话上执行 shell 命令，返回 stdout/stderr/exit code |
| `sftp_home` / `sftp_list` / `sftp_stat` / `sftp_read` | 远端浏览与读文件 |
| `sftp_write` / `sftp_mkdir` / `sftp_delete` / `sftp_rename` / `sftp_chmod` | 远端写入与元操作（落审计） |
| `sftp_upload` / `sftp_download` | 文件传输：入队后台传输队列执行（与 UI 传输面板共享，**无大小上限**；`wait_seconds` 可同步等待终态） |
| `sftp_transfer_list` | 查询传输进度 / 状态 / 错误 |

**权限模型**（两层，覆盖优先）：

1. 全局默认：设置 → MCP → 工具权限，5 个分组开关（list_sessions / ssh_exec / SFTP 读取 / SFTP 写入 / SFTP 传输）
2. 会话覆盖：会话编辑器 → MCP 权限页签，每组三态（跟随全局 / 允许 / 禁止）——生产机禁 `ssh_exec`、测试机全开，互不干扰

配置示例（`.omp/mcp.json`，Claude Code 同构）：

```json
{
  "mcpServers": {
    "myssh": {
      "type": "http",
      "url": "http://127.0.0.1:17345/mcp",
      "headers": { "Authorization": "Bearer <令牌>" }
    }
  }
}
```

## 路线图

- Agent CLI：可脚本化的命令行接口（规划中）

## 技术栈

| 层       | 选型                                                |
| -------- | --------------------------------------------------- |
| 桌面     | Tauri 2.11（Rust + WebView2）                        |
| 前端     | TypeScript + React 19 + Vite + zustand + Tailwind 4 |
| 终端     | xterm.js 6 + WebGL addon（canvas 兜底）              |
| SSH      | russh 0.62 / russh-sftp 2.4                         |
| 本地终端 | portable-pty（ConPTY）                              |
| 存储     | SQLite（sqlx）+ DPAPI 凭据保险库                     |
| 异步     | tokio                                               |

## 构建

要求：Rust stable、Node.js 20+、Windows 10/11（WebView2 Runtime）。

```bash
# 前端
cd app/ui
npm install
npm run build

# 桌面应用（release 二进制）
cargo build -p app --release
# 产物：target/release/app.exe

# 安装包（NSIS + MSI）——仓库根目录执行
app/ui/node_modules/.bin/tauri.cmd build
# 产物：target/release/bundle/
```

开发模式（热更新）：

```bash
cd app/ui && npm run dev        # Vite dev server
cargo run -p app                # 另开一个终端
```

## 测试与质量

```bash
# Rust（仓库根）
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# 前端（app/ui）
npm run lint
npm test
```

工程红线：非测试代码禁 `unwrap`/`expect`（豁免须附 `// SAFETY:` 注释）、禁 `unsafe`；终端输出走 Tauri IPC Channel（二进制，非 JSON 事件）；隧道 / SFTP 各占独立 SSH 连接，与交互终端传输层隔离。

## 项目结构

```
crates/
  app/          Tauri 壳：命令层、IPC 装配
  core-ssh/     SSH 协议核心（连接、认证、通道）
  core-tunnel/  端口转发
  core-sftp/    SFTP 与传输队列
  core-monitor/ 服务器监控采样
  core-store/   SQLite 持久化、凭据保险库、导入导出
  core-policy/  策略
  cli/          Agent CLI（规划中）
app/ui/         前端（React + xterm）
```

## 数据位置

`%LOCALAPPDATA%\myssh\`：`myssh.db`（SQLite：会话、隧道、传输历史）、凭据（DPAPI 加密，绑定当前 Windows 用户与机器）、`known_hosts`、`logs/`。

## 许可证

[MIT License](LICENSE)
