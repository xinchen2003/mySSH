//! OpenSSH 客户端配置导入（~/.ssh/config）。
//!
//! 取舍：Host 含通配符（* ?）的块跳过（属模式匹配非具体主机）；
//! 有 IdentityFile → publickey（存路径引用）；无 → agent（最贴近 ssh CLI 默认行为）。
//! PuTTY（注册表）/Xshell（二进制 .xsh）导入在 M5。

use crate::error::StoreError;
use crate::session::{AuthType, SessionRecord};
use crate::Store;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportOutcome {
    pub imported: usize,
    pub skipped: usize,
    /// ProxyJump 别名未匹配到任何已存会话（引用被剔除，需用户手工补链）
    pub unresolved_jumps: usize,
}

#[derive(Debug, Clone)]
pub struct ParseOutcome {
    pub records: Vec<SessionRecord>,
    /// 通配 Host 块 / 缺 User 的块
    pub skipped: usize,
}

/// 解析 OpenSSH config 文本为会话草稿（id 以 `ssh-{alias}` 生成）
pub fn parse_openssh_config(text: &str, home: &str) -> ParseOutcome {
    let mut out = Vec::new();
    let mut skipped = 0usize;
    let mut cur: Option<Draft> = None;
    // 通配-only 块标记：出现即计 skipped（不覆盖前块的收尾）
    let mut cur_counted = false;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = match line.split_once(char::is_whitespace) {
            Some((k, v)) => (k.to_ascii_lowercase(), v.trim()),
            None => (line.to_ascii_lowercase(), ""),
        };
        if key == "host" {
            if let Some(d) = cur.take() {
                match d.finish(home) {
                    Some(rec) => out.push(rec),
                    None => skipped += 1,
                }
            }
            if cur_counted {
                skipped += 1;
                cur_counted = false;
            }
            // Host 可空格分隔多个模式；取第一个非通配别名
            let alias = value
                .split_whitespace()
                .find(|p| !p.contains('*') && !p.contains('?'));
            match alias {
                Some(a) => {
                    cur = Some(Draft {
                        alias: a.to_string(),
                        ..Default::default()
                    });
                }
                None => {
                    cur = None;
                    cur_counted = true; // 通配-only 块
                }
            }
            continue;
        }
        let Some(d) = cur.as_mut() else { continue };
        match key.as_str() {
            "hostname" => d.host = value.to_string(),
            "port" => d.port = value.parse().ok(),
            "user" => d.user = value.to_string(),
            "identityfile" => d.identity_file = Some(value.to_string()),
            // ProxyJump 别名 → 暂存 ssh-<别名> 形式的跳板引用；命令层落库后
            // 校验存在性，悬空引用剔除并计入 unresolved_jumps
            "proxyjump" => {
                let alias = value.trim();
                if !alias.is_empty() && alias != "none" {
                    d.proxy_jump = Some(alias.to_string());
                }
            }
            _ => {} // 其余指令（ForwardAgent/…）不映射
        }
    }
    if let Some(d) = cur.take() {
        match d.finish(home) {
            Some(rec) => out.push(rec),
            None => skipped += 1,
        }
    }
    if cur_counted {
        skipped += 1;
    }
    ParseOutcome {
        records: out,
        skipped,
    }
}

#[derive(Default)]
struct Draft {
    alias: String,
    host: String,
    port: Option<u16>,
    user: String,
    identity_file: Option<String>,
    proxy_jump: Option<String>,
}

impl Draft {
    fn finish(self, home: &str) -> Option<SessionRecord> {
        // HostName 缺省 = 别名本身（OpenSSH 语义）
        let host = if self.host.is_empty() {
            self.alias.clone()
        } else {
            self.host
        };
        if self.user.is_empty() {
            return None; // 无用户名进不了我们的模型 → 记 skipped
        }
        let key_path = self.identity_file.map(|p| expand_home(&p, home));
        Some(SessionRecord {
            id: format!("ssh-{}", self.alias),
            name: self.alias,
            host,
            port: self.port.unwrap_or(22),
            user: self.user,
            auth_type: if key_path.is_some() {
                AuthType::PublicKey
            } else {
                AuthType::Agent
            },
            key_path,
            jump_chain: self
                .proxy_jump
                .map(|a| vec![format!("ssh-{a}")])
                .unwrap_or_default(),
            group_path: String::new(),
            tags: vec!["imported".into()],
            command: None,
            created_at: String::new(),
            updated_at: String::new(),
        })
    }
}

fn expand_home(path: &str, home: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        format!("{home}/{rest}")
    } else if let Some(rest) = path.strip_prefix("~\\") {
        format!("{home}\\{rest}")
    } else {
        path.to_string()
    }
}

/// 导入到库：幂等 upsert（重复导入更新同名记录）。
/// ProxyJump 别名在所有记录落库后解析：存在的保留，悬空的剔除并计数。
pub async fn import_openssh(
    store: &Store,
    text: &str,
    home: &str,
) -> Result<ImportOutcome, StoreError> {
    let ParseOutcome { records, skipped } = parse_openssh_config(text, home);
    let imported = records.len();
    for rec in &records {
        store.sessions().upsert(rec).await?;
    }
    // 二遍：剔除悬空跳板引用（目标别名未导入且库中也不存在）
    let mut unresolved_jumps = 0usize;
    for rec in records {
        if rec.jump_chain.is_empty() {
            continue;
        }
        let mut resolved = Vec::with_capacity(rec.jump_chain.len());
        for hop in &rec.jump_chain {
            match store.sessions().get(hop).await {
                Ok(_) => resolved.push(hop.clone()),
                Err(_) => unresolved_jumps += 1,
            }
        }
        if resolved.len() != rec.jump_chain.len() {
            let mut fixed = rec.clone();
            fixed.jump_chain = resolved;
            store.sessions().upsert(&fixed).await?;
        }
    }
    Ok(ImportOutcome {
        imported,
        skipped,
        unresolved_jumps,
    })
}
