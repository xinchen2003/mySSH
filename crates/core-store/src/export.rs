//! 配置导出/导入（M2 收口）。
//!
//! 两种形态（规格书 M2）：
//! - 明文 JSON：会话 + 隧道定义，**绝不含秘密材料**（密码/passphrase 不出保险库）
//! - 加密 JSON：同上 + 凭据（base64），整体 AES-256-GCM；密钥 = Argon2id(口令)
//!
//! 包络自描述：`encrypted` 标记决定导入路径；加密包络带 KDF 参数可跨机还原。

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use argon2::Argon2;
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::error::StoreError;
use crate::session::SessionRecord;
use crate::tunnel::TunnelRecord;
use crate::{CredentialKind, Store};

const ARGON2_M_KIB: u32 = 64 * 1024;
const ARGON2_T: u32 = 3;
const ARGON2_P: u32 = 4;

// ---- C7 导入输入硬上限 ----
/// 包络文本最大 64 MiB
const MAX_ENVELOPE_BYTES: usize = 64 * 1024 * 1024;
/// 解密后 payload 最大 128 MiB
const MAX_PAYLOAD_BYTES: usize = 128 * 1024 * 1024;
const MAX_SESSIONS: usize = 100_000;
const MAX_TUNNELS: usize = 100_000;
const MAX_CREDENTIALS: usize = 300_000;
const SALT_MIN_BYTES: usize = 16;
const SALT_MAX_BYTES: usize = 64;
/// AES-GCM nonce 严格 12 字节
const NONCE_BYTES: usize = 12;
/// Argon2 内存 8~256 MiB（桌面定位下调，原评审上限 1 GiB）
const ARGON2_M_MIN_KIB: u32 = 8 * 1024;
const ARGON2_M_MAX_KIB: u32 = 256 * 1024;
const ARGON2_T_MIN: u32 = 1;
const ARGON2_T_MAX: u32 = 10;
const ARGON2_P_MIN: u32 = 1;
const ARGON2_P_MAX: u32 = 16;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CredEntry {
    session_id: String,
    kind: String,
    /// base64
    secret: String,
}

/// 导出载荷（加密形态的明文）
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Payload {
    sessions: Vec<SessionRecord>,
    tunnels: Vec<TunnelRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    credentials: Vec<CredEntry>,
}

/// 导出/导入包络
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    v: u32,
    app: String,
    encrypted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kdf: Option<KdfInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nonce: Option<String>,
    /// 明文形态 = Payload JSON；加密形态 = base64 密文
    data: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KdfInfo {
    algo: String,
    salt: String,
    m_kib: u32,
    t: u32,
    p: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigImportOutcome {
    pub sessions: usize,
    pub tunnels: usize,
    pub credentials: usize,
}

/// 明文导出（无秘密材料）
pub async fn export_plain(store: &Store) -> Result<String, StoreError> {
    let payload = Payload {
        sessions: store.sessions().list().await?,
        tunnels: store.tunnels().list().await?,
        credentials: vec![],
    };
    let env = Envelope {
        v: 1,
        app: "myssh".into(),
        encrypted: false,
        kdf: None,
        nonce: None,
        data: serde_json::to_value(payload).map_err(|e| StoreError::Corrupt(e.to_string()))?,
    };
    serde_json::to_string_pretty(&env).map_err(|e| StoreError::Corrupt(e.to_string()))
}

/// 加密导出（含凭据）；AES-256-GCM，密钥 Argon2id(口令, 随机盐)
pub async fn export_encrypted(store: &Store, passphrase: &[u8]) -> Result<String, StoreError> {
    let mut credentials = Vec::new();
    for s in store.sessions().list().await? {
        // 迁移 0009 起每会话可并存多条凭据（password / key_passphrase / su_password）
        for (kind, sec) in store.credentials().get_all(&s.id).await? {
            credentials.push(CredEntry {
                session_id: s.id.clone(),
                kind: kind.as_str().into(),
                secret: base64::engine::general_purpose::STANDARD.encode(sec.expose()),
            });
        }
    }
    let payload = Payload {
        sessions: store.sessions().list().await?,
        tunnels: store.tunnels().list().await?,
        credentials,
    };
    let plain = serde_json::to_vec(&payload).map_err(|e| StoreError::Corrupt(e.to_string()))?;

    let salt: [u8; 16] = rand::random();
    let nonce_bytes: [u8; 12] = rand::random();
    let key = derive_key(passphrase, &salt, ARGON2_M_KIB, ARGON2_T, ARGON2_P)?;
    let gcm_key = Key::<Aes256Gcm>::try_from(&key[..])
        .map_err(|_| StoreError::Crypto("密钥长度错误".into()))?;
    let cipher = Aes256Gcm::new(&gcm_key);
    let nonce = Nonce::try_from(&nonce_bytes[..])
        .map_err(|_| StoreError::Crypto("nonce 长度错误".into()))?;
    let blob = cipher
        .encrypt(&nonce, plain.as_slice())
        .map_err(|e| StoreError::Crypto(format!("加密失败: {e}")))?;

    let env = Envelope {
        v: 1,
        app: "myssh".into(),
        encrypted: true,
        kdf: Some(KdfInfo {
            algo: "argon2id".into(),
            salt: base64::engine::general_purpose::STANDARD.encode(salt),
            m_kib: ARGON2_M_KIB,
            t: ARGON2_T,
            p: ARGON2_P,
        }),
        nonce: Some(base64::engine::general_purpose::STANDARD.encode(nonce_bytes)),
        data: serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(blob)),
    };
    serde_json::to_string_pretty(&env).map_err(|e| StoreError::Corrupt(e.to_string()))
}

/// 导入（自动识别明文/加密包络；加密需口令）。幂等 upsert。
///
/// 顺序：解析 → 版本/KDF/字段/引用校验（全部事务外）→ DPAPI 加密（事务外）
/// → begin → 批量写入 → commit；Argon2/AES/DPAPI/大 JSON 一律不进事务。
/// 原子性边界（C7）：仅覆盖 SQLite sessions/tunnels/credentials 表；
/// DPAPI 只在事务外生成加密 blob，未来外部 credential provider 不承诺跨系统 ACID。
pub async fn import_config(
    store: &Store,
    text: &str,
    passphrase: Option<&[u8]>,
) -> Result<ConfigImportOutcome, StoreError> {
    if text.len() > MAX_ENVELOPE_BYTES {
        return Err(StoreError::Validation(format!(
            "配置包络超过 {} MiB 上限",
            MAX_ENVELOPE_BYTES / 1024 / 1024
        )));
    }
    let env: Envelope =
        serde_json::from_str(text).map_err(|e| StoreError::Corrupt(format!("包络解析: {e}")))?;
    if env.app != "myssh" || env.v != 1 {
        return Err(StoreError::Corrupt("非 mySSH v1 配置包络".into()));
    }
    let payload: Payload = if env.encrypted {
        let pass = passphrase.ok_or_else(|| StoreError::Corrupt("加密配置需要口令".into()))?;
        let kdf = env
            .kdf
            .as_ref()
            .ok_or_else(|| StoreError::Corrupt("缺 KDF 参数".into()))?;
        if kdf.algo != "argon2id" {
            return Err(StoreError::Corrupt(format!("未知 KDF {}", kdf.algo)));
        }
        // KDF 参数边界必须在派生前校验：恶意包络可用超大 m/t/p 放大内存与 CPU 消耗
        if !(ARGON2_M_MIN_KIB..=ARGON2_M_MAX_KIB).contains(&kdf.m_kib) {
            return Err(StoreError::Validation(format!(
                "KDF 内存参数 {} KiB 超出 {}~{} MiB 允许范围",
                kdf.m_kib,
                ARGON2_M_MIN_KIB / 1024,
                ARGON2_M_MAX_KIB / 1024
            )));
        }
        if !(ARGON2_T_MIN..=ARGON2_T_MAX).contains(&kdf.t) {
            return Err(StoreError::Validation(format!(
                "KDF 迭代次数 {} 超出 {ARGON2_T_MIN}~{ARGON2_T_MAX} 允许范围",
                kdf.t
            )));
        }
        if !(ARGON2_P_MIN..=ARGON2_P_MAX).contains(&kdf.p) {
            return Err(StoreError::Validation(format!(
                "KDF 并行度 {} 超出 {ARGON2_P_MIN}~{ARGON2_P_MAX} 允许范围",
                kdf.p
            )));
        }
        let salt = b64dec(&kdf.salt)?;
        if !(SALT_MIN_BYTES..=SALT_MAX_BYTES).contains(&salt.len()) {
            return Err(StoreError::Validation(format!(
                "salt 长度须为 {SALT_MIN_BYTES}~{SALT_MAX_BYTES} 字节，实际 {}",
                salt.len()
            )));
        }
        let nonce = b64dec(
            env.nonce
                .as_ref()
                .ok_or_else(|| StoreError::Corrupt("缺 nonce".into()))?,
        )?;
        if nonce.len() != NONCE_BYTES {
            return Err(StoreError::Validation(format!(
                "AES-GCM nonce 须严格为 {NONCE_BYTES} 字节，实际 {}",
                nonce.len()
            )));
        }
        let blob = b64dec(
            env.data
                .as_str()
                .ok_or_else(|| StoreError::Corrupt("密文形态错误".into()))?,
        )?;
        let key = derive_key(pass, &salt, kdf.m_kib, kdf.t, kdf.p)?;
        let gcm_key = Key::<Aes256Gcm>::try_from(&key[..])
            .map_err(|_| StoreError::Crypto("密钥长度错误".into()))?;
        let cipher = Aes256Gcm::new(&gcm_key);
        let nonce_arr =
            Nonce::try_from(&nonce[..]).map_err(|_| StoreError::Crypto("nonce 长度错误".into()))?;
        let plain = cipher
            .decrypt(&nonce_arr, blob.as_slice())
            .map_err(|_| StoreError::Corrupt("解密失败（口令错误或数据损坏）".into()))?;
        if plain.len() > MAX_PAYLOAD_BYTES {
            return Err(StoreError::Validation(format!(
                "解密载荷超过 {} MiB 上限",
                MAX_PAYLOAD_BYTES / 1024 / 1024
            )));
        }
        serde_json::from_slice(&plain).map_err(|e| StoreError::Corrupt(e.to_string()))?
    } else {
        serde_json::from_value(env.data).map_err(|e| StoreError::Corrupt(e.to_string()))?
    };

    validate_payload(store, &payload).await?;

    // 凭据落库准备（全部事务外）：kind 解析 + base64 解码 + DPAPI 加密 blob。
    let mut creds: Vec<(String, CredentialKind, Vec<u8>)> =
        Vec::with_capacity(payload.credentials.len());
    for c in &payload.credentials {
        let kind = CredentialKind::parse(&c.kind)?;
        let secret = crate::Secret::new(b64dec(&c.secret)?);
        let blob = crate::cred::protect(secret.expose())?;
        creds.push((c.session_id.clone(), kind, blob));
    }

    // 批量写入：单事务保证整体原子性，任一失败回滚、无部分导入
    let mut tx = store
        .pool()
        .begin()
        .await
        .map_err(|e| StoreError::Query(e.to_string()))?;
    for rec in &payload.sessions {
        store.sessions().upsert_tx(&mut tx, rec).await?;
    }
    for t in &payload.tunnels {
        store.tunnels().upsert_tx(&mut tx, t).await?;
    }
    for (session_id, kind, blob) in &creds {
        store
            .credentials()
            .put_blob_tx(&mut tx, session_id, *kind, blob)
            .await?;
    }
    tx.commit()
        .await
        .map_err(|e| StoreError::Query(e.to_string()))?;

    Ok(ConfigImportOutcome {
        sessions: payload.sessions.len(),
        tunnels: payload.tunnels.len(),
        credentials: creds.len(),
    })
}

/// 事务外校验（C7 输入上限）：条数上限、ID 重复、隧道字段口径、引用完整性。
/// 引用集合 = 载荷内会话 ∪ 库内已存在会话；引用缺失整体拒绝，
/// 不再依赖写入期 FK 报错造成的中途失败。
async fn validate_payload(store: &Store, payload: &Payload) -> Result<(), StoreError> {
    if payload.sessions.len() > MAX_SESSIONS {
        return Err(StoreError::Validation(format!(
            "会话条数 {} 超过上限 {MAX_SESSIONS}",
            payload.sessions.len()
        )));
    }
    if payload.tunnels.len() > MAX_TUNNELS {
        return Err(StoreError::Validation(format!(
            "隧道条数 {} 超过上限 {MAX_TUNNELS}",
            payload.tunnels.len()
        )));
    }
    if payload.credentials.len() > MAX_CREDENTIALS {
        return Err(StoreError::Validation(format!(
            "凭据条数 {} 超过上限 {MAX_CREDENTIALS}",
            payload.credentials.len()
        )));
    }

    // ID 重复检测：upsert 语义下重复 id 会静默覆盖，导入视为输入错误
    let mut session_ids = std::collections::HashSet::with_capacity(payload.sessions.len());
    for s in &payload.sessions {
        if s.id.trim().is_empty() {
            return Err(StoreError::Validation("会话 id 不能为空".into()));
        }
        if !session_ids.insert(s.id.as_str()) {
            return Err(StoreError::Validation(format!("会话 id 重复: {}", s.id)));
        }
    }
    let mut tunnel_ids = std::collections::HashSet::with_capacity(payload.tunnels.len());
    for t in &payload.tunnels {
        crate::tunnel::validate(t)?;
        if !tunnel_ids.insert(t.id.as_str()) {
            return Err(StoreError::Validation(format!("隧道 id 重复: {}", t.id)));
        }
    }
    let mut cred_keys = std::collections::HashSet::with_capacity(payload.credentials.len());
    for c in &payload.credentials {
        if !cred_keys.insert((c.session_id.as_str(), c.kind.as_str())) {
            return Err(StoreError::Validation(format!(
                "凭据重复: {} ({})",
                c.session_id, c.kind
            )));
        }
    }

    // 引用完整性
    let mut known = store.sessions().list_ids().await?;
    known.extend(payload.sessions.iter().map(|s| s.id.clone()));
    for t in &payload.tunnels {
        if !known.contains(&t.session_id) {
            return Err(StoreError::Validation(format!(
                "隧道 {} 引用了不存在的会话 {}",
                t.id, t.session_id
            )));
        }
    }
    for c in &payload.credentials {
        if !known.contains(&c.session_id) {
            return Err(StoreError::Validation(format!(
                "凭据引用了不存在的会话 {}",
                c.session_id
            )));
        }
    }
    Ok(())
}

fn derive_key(
    passphrase: &[u8],
    salt: &[u8],
    m_kib: u32,
    t: u32,
    p: u32,
) -> Result<[u8; 32], StoreError> {
    let params = argon2::Params::new(m_kib, t, p, Some(32))
        .map_err(|e| StoreError::Crypto(format!("KDF 参数: {e}")))?;
    let argon = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = [0u8; 32];
    argon
        .hash_password_into(passphrase, salt, &mut key)
        .map_err(|e| StoreError::Crypto(format!("KDF 派生: {e}")))?;
    Ok(key)
}

fn b64dec(s: &str) -> Result<Vec<u8>, StoreError> {
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| StoreError::Corrupt(format!("base64: {e}")))
}
