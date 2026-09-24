//! 会话生命周期事件：变更失效扇出的唯一发出者与订阅登记处。
//!
//! 「会话改了/删了 ⇒ 谁要跟着动」此前分散在 session 命令里手工编排
//! （且 group_delete 漏了 sftp/exec ctx 摘除）；现在命令只写库 + publish，
//! 各 pool 在 [`wire`] 登记消费。时序不变量：
//!
//! - `Deleting` 在库删除**前**发出——需要读库定义的消费方在此停手；
//! - `Deleted` 在 FK 级联后发出——纯内存消费方（ctx 池）在此清理。
//!
//! 隧道停启不读库定义：运行条目自持 session_id（core-tunnel `TunnelSpec`），
//! 级联时序约束对隧道消费方不成立。

use std::sync::Arc;

use tokio::sync::broadcast;

/// 会话变更事件（broadcast 载荷须 Clone）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionChange {
    /// 配置已写入库：消费方按新配置重建/重启
    Upserted(String),
    /// 即将从库删除（定义此刻仍可读）
    Deleting(Vec<String>),
    /// 已从库删除（凭据/隧道定义随 FK 级联）
    Deleted(Vec<String>),
}

#[derive(Clone)]
pub struct SessionEvents {
    tx: broadcast::Sender<SessionChange>,
}

impl Default for SessionEvents {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionEvents {
    pub fn new() -> Self {
        // 管理面低频事件；积压即异常（消费方 Lagged warn 后按现状自愈）
        let (tx, _) = broadcast::channel(64);
        Self { tx }
    }

    pub fn publish(&self, change: SessionChange) {
        // 无订阅者（未 wire 的上下文）不算失败
        let _ = self.tx.send(change);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SessionChange> {
        self.tx.subscribe()
    }
}

/// 消费方登记处：新增依赖方 = 在此加一个订阅任务，不再改任何 session 命令。
pub fn wire(
    events: &SessionEvents,
    store: Arc<core_store::Store>,
    tunnel_mgr: Arc<core_tunnel::TunnelManager>,
    sftp: Arc<crate::sftp::SftpManagerState>,
    exec: Arc<crate::exec::ExecManagerState>,
) {
    // 隧道：改后按新配置重启（定义在库可读）；删除按运行条目的会话归属停
    let mut rx = events.subscribe();
    tauri::async_runtime::spawn(async move {
        while let Some(change) = recv(&mut rx).await {
            match change {
                SessionChange::Upserted(id) => {
                    crate::tunnels::restart_session_tunnels(tunnel_mgr.clone(), store.clone(), &id)
                        .await;
                }
                SessionChange::Deleting(ids) | SessionChange::Deleted(ids) => {
                    for sid in ids {
                        stop_tunnels_of_session(&tunnel_mgr, &sid).await;
                    }
                }
            }
        }
    });
    // SFTP ctx 池：改/删都摘除（下次 ensure 按现状重建）
    let mut rx = events.subscribe();
    tauri::async_runtime::spawn(async move {
        while let Some(change) = recv(&mut rx).await {
            match change {
                SessionChange::Upserted(id) => sftp.drop_ctx(&id),
                SessionChange::Deleted(ids) => {
                    for id in &ids {
                        sftp.drop_ctx(id);
                    }
                }
                SessionChange::Deleting(_) => {}
            }
        }
    });
    // Exec ctx 池：同 SFTP 策略
    let mut rx = events.subscribe();
    tauri::async_runtime::spawn(async move {
        while let Some(change) = recv(&mut rx).await {
            match change {
                SessionChange::Upserted(id) => exec.drop_ctx(&id),
                SessionChange::Deleted(ids) => {
                    for id in &ids {
                        exec.drop_ctx(id);
                    }
                }
                SessionChange::Deleting(_) => {}
            }
        }
    });
}

/// 按运行条目的会话归属停隧道（不读库定义——删除场景 FK 级联后已查不到）
async fn stop_tunnels_of_session(mgr: &Arc<core_tunnel::TunnelManager>, session_id: &str) {
    let running: Vec<String> = mgr
        .list()
        .into_iter()
        .filter(|t| t.session_id == session_id)
        .map(|t| t.id)
        .collect();
    for id in running {
        match mgr.stop(&id).await {
            Ok(()) | Err(core_tunnel::TunnelError::NotFound(_)) => {}
            Err(e) => tracing::warn!(tunnel = %id, error = %e, "会话失效停止隧道失败"),
        }
    }
}

/// Lagged 不算终局：跳过积压继续（消费方随后按现状自愈）；Closed 才退出
async fn recv(rx: &mut broadcast::Receiver<SessionChange>) -> Option<SessionChange> {
    loop {
        match rx.recv().await {
            Ok(change) => return Some(change),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(skipped = n, "会话事件消费滞后，积压事件已丢弃");
            }
            Err(broadcast::error::RecvError::Closed) => return None,
        }
    }
}
