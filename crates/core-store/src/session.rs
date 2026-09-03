//! 会话档案仓储：sessions 表 CRUD（无秘密材料）。

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

use crate::error::StoreError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthType {
    Password,
    /// DB/CLI 一律 "publickey"（与 OpenSSH 术语一致，不做 kebab 拆词）
    #[serde(rename = "publickey")]
    PublicKey,
    KeyboardInteractive,
    Agent,
}

impl AuthType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::PublicKey => "publickey",
            Self::KeyboardInteractive => "keyboard-interactive",
            Self::Agent => "agent",
        }
    }

    fn parse(s: &str) -> Result<Self, StoreError> {
        match s {
            "password" => Ok(Self::Password),
            "publickey" => Ok(Self::PublicKey),
            "keyboard-interactive" => Ok(Self::KeyboardInteractive),
            "agent" => Ok(Self::Agent),
            other => Err(StoreError::Corrupt(format!("auth_type: {other}"))),
        }
    }
}
/// 会话类型：ssh = 远程 SSH；local = 本机 PTY（ConPTY），host/port/username/auth_type 存占位值
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SessionKind {
    #[default]
    Ssh,
    Local,
}

impl SessionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ssh => "ssh",
            Self::Local => "local",
        }
    }

    fn parse(s: &str) -> Result<Self, StoreError> {
        match s {
            "ssh" => Ok(Self::Ssh),
            "local" => Ok(Self::Local),
            other => Err(StoreError::Corrupt(format!("kind: {other}"))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    pub id: String,
    pub name: String,
    /// 会话类型（旧导出包络无此字段，默认 ssh）
    #[serde(default)]
    pub kind: SessionKind,
    pub host: String,
    pub port: u16,
    #[serde(rename = "username")]
    pub user: String,
    pub auth_type: AuthType,
    pub key_path: Option<String>,
    /// local：启动的 shell（powershell|pwsh|cmd 或自定义路径）；None = 自动（pwsh→powershell→cmd）
    #[serde(default)]
    pub shell: Option<String>,
    /// local：启动目录；None = 用户主目录
    #[serde(default)]
    pub workdir: Option<String>,
    /// ProxyJump 链：session id 数组（就近→最远）；空 = 直连
    #[serde(default)]
    pub jump_chain: Vec<String>,
    pub group_path: String,
    /// 侧栏色点（hex，如 '#e5484d'）；None = 无色
    #[serde(default)]
    pub color: Option<String>,
    /// 终端编码（encoding_rs 标签；'utf-8' = 直通不转码）。旧导出包络无此字段，默认 utf-8
    #[serde(default = "default_encoding")]
    pub encoding: String,
    /// 登录后切换用户（su）的目标用户名；None/空 = 不切换。密码存 credentials(kind=su_password)
    #[serde(default)]
    pub su_user: Option<String>,
    pub tags: Vec<String>,
    pub command: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

pub struct SessionRepo {
    pool: SqlitePool,
}

const LIST_SQL: &str = "SELECT id,name,kind,host,shell,workdir,port,username,auth_type,key_path,group_path,tags,command,jump_chain,created_at,updated_at,color,encoding,su_user FROM sessions ORDER BY group_path, name";
const GET_SQL: &str = "SELECT id,name,kind,host,shell,workdir,port,username,auth_type,key_path,group_path,tags,command,jump_chain,created_at,updated_at,color,encoding,su_user FROM sessions WHERE id = ?";

/// 终端编码缺省值（旧数据/旧导出兼容）
fn default_encoding() -> String {
    "utf-8".into()
}

impl SessionRepo {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn list(&self) -> Result<Vec<SessionRecord>, StoreError> {
        let rows = sqlx::query(LIST_SQL)
            .fetch_all(&self.pool)
            .await
            .map_err(db)?;
        rows.iter().map(row_to_record).collect()
    }

    pub async fn get(&self, id: &str) -> Result<SessionRecord, StoreError> {
        let row = sqlx::query(GET_SQL)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db)?
            .ok_or_else(|| StoreError::NotFound(format!("session {id}")))?;
        row_to_record(&row)
    }

    /// 新建或更新（按 id 是否存在区分）；返回最终记录
    pub async fn upsert(&self, rec: &SessionRecord) -> Result<SessionRecord, StoreError> {
        let tags =
            serde_json::to_string(&rec.tags).map_err(|e| StoreError::Corrupt(e.to_string()))?;
        let jump_chain = serde_json::to_string(&rec.jump_chain)
            .map_err(|e| StoreError::Corrupt(e.to_string()))?;
        sqlx::query(
            "INSERT INTO sessions (id,name,kind,host,shell,workdir,port,username,auth_type,key_path,group_path,color,encoding,su_user,tags,command,jump_chain,updated_at)
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,datetime('now'))
             ON CONFLICT(id) DO UPDATE SET
               name=excluded.name, kind=excluded.kind, host=excluded.host, shell=excluded.shell, workdir=excluded.workdir, port=excluded.port,
               username=excluded.username, auth_type=excluded.auth_type,
               key_path=excluded.key_path, group_path=excluded.group_path,
               color=excluded.color, encoding=excluded.encoding, su_user=excluded.su_user,
               tags=excluded.tags, command=excluded.command,
               jump_chain=excluded.jump_chain, updated_at=datetime('now')",
        )
        .bind(&rec.id)
        .bind(&rec.name)
        .bind(rec.kind.as_str())
        .bind(&rec.host)
        .bind(&rec.shell)
        .bind(&rec.workdir)
        .bind(rec.port as i64)
        .bind(&rec.user)
        .bind(rec.auth_type.as_str())
        .bind(&rec.key_path)
        .bind(&rec.group_path)
        .bind(&rec.color)
        .bind(&rec.encoding)
        .bind(&rec.su_user)
        .bind(tags)
        .bind(&rec.command)
        .bind(jump_chain)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        self.get(&rec.id).await
    }

    pub async fn delete(&self, id: &str) -> Result<(), StoreError> {
        let res = sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        if res.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!("session {id}")));
        }
        Ok(())
    }
    /// 分组重命名/移动（含子树）：事务性前缀改写。
    /// 循环防护：目标不得落在源子树内（如 a/b → a/b/c）；new='' 表示移到未分组。
    /// 返回受影响会话数。
    pub async fn group_rename(&self, old: &str, new: &str) -> Result<u64, StoreError> {
        validate_group_path(old)?;
        validate_group_path(new)?;
        if old.is_empty() {
            return Err(StoreError::Validation("不能重命名未分组根".into()));
        }
        if old == new {
            return Ok(0);
        }
        if new.starts_with(old) && new.as_bytes().get(old.len()) == Some(&b'/') {
            return Err(StoreError::Validation(format!(
                "不能把分组移动到它自己的子分组内: {old} → {new}"
            )));
        }
        // SQLite substr 按字符计：中文路径必须用字符数而非字节数
        let old_chars = old.chars().count() as i64;
        let mut tx = self.pool.begin().await.map_err(db)?;
        let res = sqlx::query(
            "UPDATE sessions
             SET group_path = CASE WHEN ?1 = '' THEN substr(group_path, ?3)
                                   ELSE ?1 || substr(group_path, ?2) END,
                 updated_at = datetime('now')
             WHERE group_path = ?4
                OR (substr(group_path, 1, ?5) = ?4 AND substr(group_path, ?2, 1) = '/')",
        )
        .bind(new) // ?1
        .bind(old_chars + 1) // ?2 跳过 old 本身（保留 '/…' 后缀）
        .bind(old_chars + 2) // ?3 new='' 时连同 '/' 一起跳过
        .bind(old) // ?4
        .bind(old_chars) // ?5
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(res.rows_affected())
    }

    /// 分组删除。with_sessions=false：直属成员移未分组、子分组上移父级；
    /// true：删除子树全部会话（凭据/隧道由 FK ON DELETE CASCADE 级联）。
    /// 事务执行；返回受影响（移动或删除）的会话数。
    pub async fn group_delete(&self, path: &str, with_sessions: bool) -> Result<u64, StoreError> {
        validate_group_path(path)?;
        if path.is_empty() {
            return Err(StoreError::Validation("不能删除未分组根".into()));
        }
        let path_chars = path.chars().count() as i64;
        let mut tx = self.pool.begin().await.map_err(db)?;
        if with_sessions {
            let res = sqlx::query(
                "DELETE FROM sessions
                 WHERE group_path = ?1
                    OR (substr(group_path, 1, ?2) = ?1 AND substr(group_path, ?2 + 1, 1) = '/')",
            )
            .bind(path)
            .bind(path_chars)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
            tx.commit().await.map_err(db)?;
            return Ok(res.rows_affected());
        }
        // 保留会话：直属成员 → 未分组；子树成员 → 父级（父为根则去掉顶层段）
        let parent = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
        let direct = sqlx::query(
            "UPDATE sessions SET group_path = '', updated_at = datetime('now')
             WHERE group_path = ?1",
        )
        .bind(path)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        let children = sqlx::query(
            "UPDATE sessions
             SET group_path = CASE WHEN ?2 = '' THEN substr(group_path, ?4)
                                   ELSE ?2 || '/' || substr(group_path, ?4) END,
                 updated_at = datetime('now')
             WHERE substr(group_path, 1, ?3) = ?1 AND substr(group_path, ?3 + 1, 1) = '/'",
        )
        .bind(path) // ?1
        .bind(parent) // ?2
        .bind(path_chars) // ?3
        .bind(path_chars + 2) // ?4 跳过 path + '/'
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(direct.rows_affected() + children.rows_affected())
    }

    /// 批量移动会话到目标分组（'' = 未分组）。事务执行；返回受影响行数。
    pub async fn move_to_group(&self, ids: &[String], group_path: &str) -> Result<u64, StoreError> {
        validate_group_path(group_path)?;
        if ids.is_empty() {
            return Ok(0);
        }
        let mut tx = self.pool.begin().await.map_err(db)?;
        let mut affected = 0u64;
        for id in ids {
            let res = sqlx::query(
                "UPDATE sessions SET group_path = ?1, updated_at = datetime('now') WHERE id = ?2",
            )
            .bind(group_path)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
            affected += res.rows_affected();
        }
        tx.commit().await.map_err(db)?;
        Ok(affected)
    }
}
/// 分组路径校验：'' 合法（未分组根）；段非空且无首尾空格；'/' 分隔
fn validate_group_path(path: &str) -> Result<(), StoreError> {
    if path.is_empty() {
        return Ok(());
    }
    for seg in path.split('/') {
        if seg.is_empty() || seg.trim() != seg {
            return Err(StoreError::Validation(format!("分组路径无效: {path:?}")));
        }
    }
    Ok(())
}

fn row_to_record(row: &sqlx::sqlite::SqliteRow) -> Result<SessionRecord, StoreError> {
    let tags_raw: String = row.get("tags");
    let jump_raw: String = row.get("jump_chain");
    Ok(SessionRecord {
        id: row.get("id"),
        name: row.get("name"),
        kind: SessionKind::parse(row.get("kind"))?,
        host: row.get("host"),
        shell: row.get("shell"),
        workdir: row.get("workdir"),
        port: row.get::<i64, _>("port") as u16,
        user: row.get("username"),
        auth_type: AuthType::parse(row.get("auth_type"))?,
        key_path: row.get("key_path"),
        group_path: row.get("group_path"),
        color: row.get("color"),
        encoding: row.get("encoding"),
        su_user: row.get("su_user"),
        tags: serde_json::from_str(&tags_raw).map_err(|e| StoreError::Corrupt(e.to_string()))?,
        jump_chain: serde_json::from_str(&jump_raw)
            .map_err(|e| StoreError::Corrupt(e.to_string()))?,
        command: row.get("command"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

fn db(e: sqlx::Error) -> StoreError {
    StoreError::Query(e.to_string())
}
