//! myssh-cli：Agent 友好的非交互 CLI（规格书 M6 形态二）。
//!
//! M0 范围：完整命令面 + 语义化退出码 + --json 信封；子命令实现随里程碑接线。
//! 设计契约见 docs/design/08-ai-interface.md 第 2 节。

mod exit_codes;

use clap::{Parser, Subcommand};
use exit_codes::ExitCode;

#[derive(Parser)]
#[command(
    name = "myssh-cli",
    version,
    about = "mySSH 的 Agent 友好 CLI：非交互、结构化输出、语义化退出码",
    long_about = "mySSH 命令行工具。\n\n\
        约定：\n\
        - stdout 只输出数据（--json 时为稳定 schema），日志/进度走 stderr\n\
        - 需要交互输入的场景（密码、主机密钥确认）必须显式传参，否则直接失败\n\
        - 每个命令支持 --timeout，绝不无限等待；--dry-run 只打印不执行\n\
        退出码：0 成功 / 1 远端失败 / 2 参数错误 / 3 连接失败 / 4 认证失败 / \
        5 权限拒绝 / 6 超时 / 7 用户取消"
)]
struct Cli {
    /// 结构化 JSON 输出（稳定 schema，golden file 契约）
    #[arg(long, global = true)]
    json: bool,
    /// 超时秒数（所有命令共享默认值）
    #[arg(long, global = true, default_value_t = 30)]
    timeout: u64,
    /// 只打印将执行的操作，不执行
    #[arg(long, global = true)]
    dry_run: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 会话管理：list / show / test
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
    /// 在指定会话执行命令
    Exec {
        /// 会话 ID 或名称
        session: String,
        /// 要执行的命令
        command: String,
        /// 远端工作目录
        #[arg(long)]
        cwd: Option<String>,
        /// stdin 来源：文件路径或 -（管道）
        #[arg(long)]
        stdin: Option<String>,
    },
    /// 文件传输与读取：get / put / cat / ls
    File {
        #[command(subcommand)]
        action: FileAction,
    },
    /// 隧道管理：start / stop / list / status
    Tunnel {
        #[command(subcommand)]
        action: TunnelAction,
    },
    /// 服务器监控快照
    Metrics { session: String },
    /// 启动内置 MCP Server
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
}

#[derive(Subcommand)]
enum SessionAction {
    /// 列出已配置会话（脱敏）
    List,
    /// 查看单个会话
    Show { session: String },
    /// 测试连通性与认证
    Test { session: String },
}

#[derive(Subcommand)]
enum FileAction {
    /// 下载远程文件
    Get {
        session: String,
        remote: String,
        local: String,
    },
    /// 上传本地文件
    Put {
        session: String,
        local: String,
        remote: String,
    },
    /// 读取远程文件到 stdout（有大小上限，二进制拒绝）
    Cat { session: String, path: String },
    /// 列目录
    Ls { session: String, path: String },
}

#[derive(Subcommand)]
enum TunnelAction {
    /// 开启端口转发
    Start {
        session: String,
        /// 转发类型
        #[arg(long, value_parser = ["local", "remote", "socks5"])]
        kind: String,
        /// 本地监听地址 host:port
        #[arg(long)]
        bind: String,
        /// 目标地址 host:port（local 必填）
        #[arg(long)]
        target: Option<String>,
    },
    Stop {
        tunnel_id: String,
    },
    List,
    Status {
        tunnel_id: String,
    },
}

#[derive(Subcommand)]
enum McpAction {
    /// 启动 MCP Server（默认 stdio）
    Serve {
        /// stdio 传输（默认）
        #[arg(long, conflicts_with = "http")]
        stdio: bool,
        /// Streamable HTTP 监听地址（仅允许 127.0.0.1，强制 token 鉴权）
        #[arg(long)]
        http: Option<String>,
        /// 权限配置档位名
        #[arg(long)]
        profile: Option<String>,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let code = match dispatch(&cli).await {
        Ok(()) => ExitCode::Ok,
        Err(e) => {
            eprintln!("myssh-cli: {e}");
            e.exit_code()
        }
    };
    std::process::exit(code as i32);
}

#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error("{0}（该子命令将在对应里程碑实现）")]
    NotImplemented(&'static str),
}

impl CliError {
    fn exit_code(&self) -> ExitCode {
        match self {
            CliError::NotImplemented(_) => ExitCode::RemoteFailure,
        }
    }
}

async fn dispatch(cli: &Cli) -> Result<(), CliError> {
    if cli.dry_run {
        eprintln!(
            "[dry-run] 将执行: {:?}",
            std::env::args().skip(1).collect::<Vec<_>>()
        );
        return Ok(());
    }
    let what: &'static str = match &cli.command {
        Commands::Session { action } => match action {
            SessionAction::List => "session list",
            SessionAction::Show { .. } => "session show",
            SessionAction::Test { .. } => "session test",
        },
        Commands::Exec { .. } => "exec",
        Commands::File { action } => match action {
            FileAction::Get { .. } => "file get",
            FileAction::Put { .. } => "file put",
            FileAction::Cat { .. } => "file cat",
            FileAction::Ls { .. } => "file ls",
        },
        Commands::Tunnel { action } => match action {
            TunnelAction::Start { .. } => "tunnel start",
            TunnelAction::Stop { .. } => "tunnel stop",
            TunnelAction::List => "tunnel list",
            TunnelAction::Status { .. } => "tunnel status",
        },
        Commands::Metrics { .. } => "metrics",
        Commands::Mcp { action } => match action {
            McpAction::Serve { .. } => "mcp serve",
        },
    };
    Err(CliError::NotImplemented(what))
}
