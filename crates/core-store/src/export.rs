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
        // 当前模型：每会话至多一条凭据（password 或 key_passphrase）
        if let Ok((kind, sec)) = store.credentials().get_with_kind(&s.id).await {
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
pub async fn import_config(
    store: &Store,
    text: &str,
    passphrase: Option<&[u8]>,
) -> Result<ConfigImportOutcome, StoreError> {
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
        let salt = b64dec(&kdf.salt)?;
        let nonce = b64dec(
            env.nonce
                .as_ref()
                .ok_or_else(|| StoreError::Corrupt("缺 nonce".into()))?,
        )?;
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
        serde_json::from_slice(&plain).map_err(|e| StoreError::Corrupt(e.to_string()))?
    } else {
        serde_json::from_value(env.data).map_err(|e| StoreError::Corrupt(e.to_string()))?
    };

    let mut outcome = ConfigImportOutcome {
        sessions: 0,
        tunnels: 0,
        credentials: 0,
    };
    for rec in &payload.sessions {
        store.sessions().upsert(rec).await?;
        outcome.sessions += 1;
    }
    for t in &payload.tunnels {
        store.tunnels().upsert(t).await?;
        outcome.tunnels += 1;
    }
    for c in &payload.credentials {
        let kind = CredentialKind::parse(&c.kind)?;
        let secret = b64dec(&c.secret)?;
        store
            .credentials()
            .put(&c.session_id, kind, &crate::Secret::new(secret))
            .await?;
        outcome.credentials += 1;
    }
    Ok(outcome)
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
