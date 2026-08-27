//! 本地 PTY（portable-pty / Windows ConPTY）：本地会话的进程承载。
//!
//! portable-pty 是阻塞 IO，各起一个 OS 线程桥接进 tokio：
//! - 读线程 → 有界 mpsc：满则线程阻塞 → 管道回压 → 子进程停写（与 SSH 窗口背压同构）；
//! - 写线程 ← std mpsc：顺序执行 输入/resize/close，避免阻塞 tokio worker；
//! - 通道任一方向断开即兜底 kill+wait 子进程，不留孤儿。

use std::io::{Read, Write};
use std::sync::Arc;

use parking_lot::Mutex;

use bytes::Bytes;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};

/// 读通道容量（块）；64 × READ_BUF ≈ 4MB 缓冲上限，超出即回压
const READ_CHAN_CAP: usize = 64;
const READ_BUF: usize = 64 * 1024;

/// 本地会话启动参数（SessionRecord 的 shell/workdir/command 三字段解析后形态）
pub struct LocalShellSpec {
    /// powershell|pwsh|cmd 或自定义可执行路径；None = 自动（pwsh → powershell）
    pub shell: Option<String>,
    /// 启动目录；None = 子进程默认（用户主目录）
    pub workdir: Option<String>,
    /// 启动命令：在 shell 内执行后保持交互（cmd /k、powershell -NoExit -Command）
    pub command: Option<String>,
}

pub struct LocalPty {
    pub reader: LocalReader,
    pub writer: LocalWriter,
    /// 实际启动的 shell 程序名（事件/UI 展示用）
    pub shell: String,
}

pub struct LocalReader {
    rx: tokio::sync::mpsc::Receiver<Bytes>,
}

impl LocalReader {
    /// 读下一块数据；None = 子进程退出/PTY 关闭
    pub async fn next_data(&mut self) -> Option<Bytes> {
        self.rx.recv().await
    }
}

enum WriterMsg {
    Data(Vec<u8>),
    Resize(u16, u16),
    Close,
}

/// 写半：Clone 后与 TermSession 的 Arc 写半同构
#[derive(Clone)]
pub struct LocalWriter {
    tx: std::sync::mpsc::Sender<WriterMsg>,
}

impl LocalWriter {
    // async 签名与 core_ssh::PtyWriter 对齐，统一 TermSession 写路径
    pub async fn write(&self, data: &[u8]) -> Result<(), String> {
        self.tx
            .send(WriterMsg::Data(data.to_vec()))
            .map_err(|_| "本地终端已关闭".to_string())
    }

    pub async fn resize(&self, cols: u32, rows: u32) -> Result<(), String> {
        self.tx
            .send(WriterMsg::Resize(cols as u16, rows as u16))
            .map_err(|_| "本地终端已关闭".to_string())
    }

    pub async fn close(&self) -> Result<(), String> {
        let _ = self.tx.send(WriterMsg::Close);
        Ok(())
    }
}

/// shell 关键词 → (可执行名, 启动命令参数拼接风格)
enum CmdStyle {
    /// cmd.exe /k <cmd>
    Cmd,
    /// powershell/pwsh -NoExit -Command <cmd>
    PowerShell,
    /// 类 bash：-ic "<cmd>; exec <shell>"（自定义路径兜底）
    Posix,
}

/// 解析 shell 选择：None → 自动探测（pwsh.exe 在 PATH 则优先，否则 powershell.exe）
fn resolve_shell(shell: Option<&str>) -> (String, CmdStyle) {
    let pick = |name: &str| -> (String, CmdStyle) {
        let lower = name.to_ascii_lowercase();
        if lower.contains("cmd") {
            (name.to_string(), CmdStyle::Cmd)
        } else if lower.contains("powershell") || lower.contains("pwsh") {
            (name.to_string(), CmdStyle::PowerShell)
        } else {
            // bash/zsh/git-bash 等自定义路径按 POSIX 风格拼启动命令
            (name.to_string(), CmdStyle::Posix)
        }
    };
    match shell {
        Some(s) if !s.trim().is_empty() => match s.trim() {
            "powershell" => pick("powershell.exe"),
            "pwsh" => pick("pwsh.exe"),
            "cmd" => pick("cmd.exe"),
            other => pick(other),
        },
        _ => {
            if path_has("pwsh.exe") {
                ("pwsh.exe".to_string(), CmdStyle::PowerShell)
            } else {
                ("powershell.exe".to_string(), CmdStyle::PowerShell)
            }
        }
    }
}

/// PATH 中是否存在可执行文件
fn path_has(exe: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(exe).is_file()))
}

/// 启动命令参数（执行后保持交互，便于 agent 退出后继续用 shell）
fn push_command_args(args: &mut Vec<String>, style: &CmdStyle, shell_prog: &str, cmdline: &str) {
    match style {
        CmdStyle::Cmd => {
            args.push("/k".into());
            args.push(cmdline.to_string());
        }
        CmdStyle::PowerShell => {
            args.push("-NoExit".into());
            args.push("-Command".into());
            args.push(cmdline.to_string());
        }
        CmdStyle::Posix => {
            args.push("-ic".into());
            args.push(format!("{cmdline}; exec {shell_prog}"));
        }
    }
}

/// 拉起本地 PTY：shell + 可选启动命令，尺寸取前端当前行列
pub fn spawn(spec: &LocalShellSpec, cols: u32, rows: u32) -> Result<LocalPty, String> {
    let (prog, style) = resolve_shell(spec.shell.as_deref());
    let mut args: Vec<String> = Vec::new();
    if let Some(cmdline) = spec.command.as_deref().filter(|c| !c.trim().is_empty()) {
        push_command_args(&mut args, &style, &prog, cmdline);
    }

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: rows as u16,
            cols: cols as u16,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("创建本地终端失败: {e}"))?;

    let mut cmd = CommandBuilder::new(&prog);
    cmd.args(args.iter().map(String::as_str));
    if let Some(dir) = spec.workdir.as_deref().filter(|d| !d.trim().is_empty()) {
        cmd.cwd(dir);
    }
    // Unix 思维的工具（部分 AI agent）按 TERM/COLORTERM 探测能力；ConPTY 全支持
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("启动 {prog} 失败: {e}"))?;
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("PTY 读通道失败: {e}"))?;
    let mut writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("PTY 写通道失败: {e}"))?;
    // master 共享持有：写线程 resize 用；守望线程在子进程退出后 drop 它——
    // Drop 触发 ClosePseudoConsole，conhost 拆除会话、关闭写端，读线程才有 EOF。
    // （实测：Windows ConPTY 在子进程退出后不会自发关闭输出管道，必须主动关伪终端）
    let master: Arc<Mutex<Option<Box<dyn portable_pty::MasterPty + Send>>>> =
        Arc::new(Mutex::new(Some(pair.master)));
    let killer = Arc::new(Mutex::new(child.clone_killer()));

    let (data_tx, data_rx) = tokio::sync::mpsc::channel::<Bytes>(READ_CHAN_CAP);
    std::thread::spawn(move || {
        let mut buf = vec![0u8; READ_BUF];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if data_tx
                        .blocking_send(Bytes::copy_from_slice(&buf[..n]))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let (msg_tx, msg_rx) = std::sync::mpsc::channel::<WriterMsg>();
    std::thread::spawn({
        let master = Arc::clone(&master);
        let killer = Arc::clone(&killer);
        move || {
            while let Ok(msg) = msg_rx.recv() {
                match msg {
                    WriterMsg::Data(d) => {
                        if writer.write_all(&d).and_then(|()| writer.flush()).is_err() {
                            break;
                        }
                    }
                    WriterMsg::Resize(c, r) => {
                        if let Some(m) = master.lock().as_ref() {
                            let _ = m.resize(PtySize {
                                rows: r,
                                cols: c,
                                pixel_width: 0,
                                pixel_height: 0,
                            });
                        }
                    }
                    WriterMsg::Close => break,
                }
            }
            // Close 或 sender 全 drop：杀子进程（守望线程负责 wait 与关伪终端）
            let _ = killer.lock().kill();
        }
    });

    // 守望线程：子进程退出（exit 自然结束或被 kill）→ drop master 关伪终端 → 读端 EOF
    std::thread::spawn(move || {
        let _ = child.wait();
        drop(master.lock().take());
    });

    Ok(LocalPty {
        reader: LocalReader { rx: data_rx },
        writer: LocalWriter { tx: msg_tx },
        shell: prog,
    })
}
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn shell_keyword_resolution() {
        let (prog, _) = resolve_shell(Some("powershell"));
        assert_eq!(prog, "powershell.exe");
        let (prog, _) = resolve_shell(Some("cmd"));
        assert_eq!(prog, "cmd.exe");
        // 自定义路径原样透传
        let (prog, style) = resolve_shell(Some("C:/Program Files/Git/bin/bash.exe"));
        assert_eq!(prog, "C:/Program Files/Git/bin/bash.exe");
        assert!(matches!(style, CmdStyle::Posix));
        // 自动探测：pwsh 或 powershell 必居其一（Windows 必有 powershell）
        let (prog, _) = resolve_shell(None);
        assert!(prog == "pwsh.exe" || prog == "powershell.exe");
    }

    #[test]
    fn command_args_stay_interactive() {
        let mut args = Vec::new();
        push_command_args(&mut args, &CmdStyle::Cmd, "cmd.exe", "claude");
        assert_eq!(args, vec!["/k", "claude"]);

        let mut args = Vec::new();
        push_command_args(
            &mut args,
            &CmdStyle::PowerShell,
            "pwsh.exe",
            "claude --continue",
        );
        assert_eq!(args, vec!["-NoExit", "-Command", "claude --continue"]);

        let mut args = Vec::new();
        push_command_args(&mut args, &CmdStyle::Posix, "bash.exe", "claude");
        assert_eq!(args, vec!["-ic", "claude; exec bash.exe"]);
    }
    /// 子进程自然退出（exit）后读端必须 EOF——终端页签据此转终态 closed
    #[tokio::test]
    async fn child_exit_yields_eof() {
        let pty = spawn(
            &LocalShellSpec {
                shell: Some("cmd".into()),
                workdir: None,
                command: None,
            },
            80,
            24,
        )
        .expect("spawn cmd");
        let mut reader = pty.reader;
        // ConPTY 启动握手：conhost 先发 DSR 查询（\x1b[6n）并等终端应答光标位置，
        // 不应答则后续输出一律挂起（xterm.js 会自动应答，测试需手动回）
        let first = tokio::time::timeout(std::time::Duration::from_secs(5), reader.next_data())
            .await
            .expect("首块输出超时");
        eprintln!(
            "first chunk: {} bytes",
            first.as_ref().map(|b| b.len()).unwrap_or(0)
        );
        pty.writer.write(b"\x1b[1;1R").await.expect("answer DSR");
        // 等到 banner 输出（>10B 的块）再发 exit，避免输入早于子进程起读被丢弃
        let mut saw_banner = false;
        for _ in 0..10 {
            match tokio::time::timeout(std::time::Duration::from_secs(3), reader.next_data()).await
            {
                Ok(Some(b)) if b.len() > 10 => {
                    saw_banner = true;
                    break;
                }
                Ok(Some(_)) => continue,
                other => panic!("banner 未到达: {:?}", other.map(|o| o.map(|b| b.len()))),
            }
        }
        assert!(saw_banner);
        pty.writer.write(b"exit\r").await.expect("write exit");
        let mut n = 0usize;
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(3), reader.next_data()).await
            {
                Ok(Some(b)) => n += b.len(),
                Ok(None) => {
                    eprintln!("EOF after {n} bytes");
                    break;
                }
                Err(_) => {
                    panic!("STALLED after {n} bytes（读端无 EOF）");
                }
            }
        }
    }
}
