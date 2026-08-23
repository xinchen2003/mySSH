//! 第三方客户端会话导入（M2 收口）：PuTTY 注册表 / Xshell .xsh / FinalShell json。
//!
//! 统一取舍：**只导入连接参数（host/port/user/密钥路径），不逆向任何密码密文**——
//! PuTTY 注册表本就不存密码；Xshell/FinalShell 密码密文格式未验证，导入后用户首次
//! 连接时补录。Xshell Method 语义码与 FinalShell 字段名为公开资料推断，未实测：`待确认`。

use crate::error::StoreError;
use crate::session::{AuthType, SessionRecord};
use crate::Store;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtImportOutcome {
    pub source: &'static str,
    pub imported: usize,
    pub skipped: usize,
}

fn draft(
    id_prefix: &str,
    name: &str,
    host: &str,
    port: u16,
    user: &str,
    key_path: Option<String>,
) -> Option<SessionRecord> {
    if host.is_empty() || user.is_empty() {
        return None;
    }
    Some(SessionRecord {
        id: format!("{id_prefix}-{}", sanitize_id(name)),
        name: name.to_string(),
        host: host.to_string(),
        port,
        user: user.to_string(),
        auth_type: if key_path.is_some() {
            AuthType::PublicKey
        } else {
            AuthType::Password
        },
        key_path,
        group_path: format!("导入/{id_prefix}"),
        tags: vec!["imported".into()],
        jump_chain: vec![],
        command: None,
        created_at: String::new(),
        updated_at: String::new(),
    })
}

fn sanitize_id(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

async fn upsert_all(
    store: &Store,
    source: &'static str,
    recs: Vec<SessionRecord>,
    skipped: usize,
) -> Result<ExtImportOutcome, StoreError> {
    let imported = recs.len();
    for r in recs {
        store.sessions().upsert(&r).await?;
    }
    Ok(ExtImportOutcome {
        source,
        imported,
        skipped,
    })
}

// ---------- PuTTY（注册表 HKCU\Software\SimonTatham\PuTTY\Sessions） ----------

/// 读取 PuTTY 注册表会话（仅 Windows；其它平台恒空）
#[cfg(windows)]
pub async fn import_putty(store: &Store) -> Result<ExtImportOutcome, StoreError> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let mut recs = Vec::new();
    let mut skipped = 0usize;
    let root = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\SimonTatham\PuTTY\Sessions")
        .map_err(|e| StoreError::Corrupt(format!("PuTTY 注册表不可读: {e}")))?;
    for name in root.enum_keys().map_while(Result::ok) {
        let Ok(k) = root.open_subkey(&name) else {
            skipped += 1;
            continue;
        };
        // PuTTY 对会话名做 %XX 编码
        let display = url_decode(&name);
        let host: String = k.get_value("HostName").unwrap_or_default();
        let port: u32 = k.get_value("PortNumber").unwrap_or(22);
        let user: String = k.get_value("UserName").unwrap_or_default();
        let key: String = k.get_value("PublicKeyFile").unwrap_or_default();
        match draft(
            "putty",
            &display,
            &host,
            port as u16,
            &user,
            if key.is_empty() { None } else { Some(key) },
        ) {
            Some(r) => recs.push(r),
            None => skipped += 1,
        }
    }
    upsert_all(store, "putty", recs, skipped).await
}

#[cfg(not(windows))]
pub async fn import_putty(_store: &Store) -> Result<ExtImportOutcome, StoreError> {
    Ok(ExtImportOutcome {
        source: "putty",
        imported: 0,
        skipped: 0,
    })
}

/// PuTTY 会话名 %XX 解码（空格=%20 等）
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------- Xshell（.xsh INI 文本） ----------

/// 解析单个 .xsh 文本。证据：NetSarang 公开 INI 结构
/// （[CONNECTION] Host/Port；[CONNECTION:AUTHENTICATION] UserName/Method）。
/// Method 语义码待确认；含 PublicKeyFile/IdentityFile 类字段则判 publickey。
pub fn parse_xsh(text: &str) -> Option<SessionRecord> {
    let mut host = String::new();
    let mut port: u16 = 22;
    let mut user = String::new();
    let mut key_path: Option<String> = None;
    let mut name = String::new();
    let mut section = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = line.trim_matches(&['[', ']'][..]).to_ascii_lowercase();
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim().to_ascii_lowercase();
        let val = v.trim();
        match (section.as_str(), key.as_str()) {
            ("connection", "host") => host = val.to_string(),
            ("connection", "port") => port = val.parse().unwrap_or(22),
            ("connection:authentication", "username") => user = val.to_string(),
            (_, "publickeyfile") | (_, "identityfile") if !val.is_empty() => {
                key_path = Some(val.to_string());
            }
            ("session", "name") | ("", "name") => name = val.to_string(),
            _ => {}
        }
    }
    if name.is_empty() {
        name = if host.is_empty() {
            String::new()
        } else {
            format!("{user}@{host}")
        };
    }
    draft("xsh", &name, &host, port, &user, key_path)
}

/// 扫描目录下全部 .xsh 导入（递归一层子目录——Xshell 会话支持文件夹分组）
pub async fn import_xshell_dir(
    store: &Store,
    dir: &std::path::Path,
) -> Result<ExtImportOutcome, StoreError> {
    let mut recs = Vec::new();
    let mut skipped = 0usize;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = match std::fs::read_dir(&d) {
            Ok(e) => e,
            Err(_) => {
                continue;
            }
        };
        for entry in entries.filter_map(Result::ok) {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("xsh")) {
                match std::fs::read_to_string(&p).ok().and_then(|t| parse_xsh(&t)) {
                    Some(mut r) => {
                        if r.name.is_empty() {
                            r.name = p
                                .file_stem()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_else(|| r.id.clone());
                        }
                        recs.push(r);
                    }
                    None => skipped += 1,
                }
            }
        }
    }
    upsert_all(store, "xshell", recs, skipped).await
}

// ---------- FinalShell（.finalshell/conn/*.json） ----------

/// 解析 FinalShell 连接 JSON。字段名（host/port/user_name/name）为公开资料
/// 推断，未对真实安装验证：`待确认`。
pub fn parse_finalshell(text: &str) -> Option<SessionRecord> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let get = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("");
    let name = {
        let n = get("name");
        if n.is_empty() {
            get("title")
        } else {
            n
        }
    };
    let host = get("host");
    let port: u16 = v
        .get("port")
        .and_then(|x| {
            x.as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| x.as_u64().map(|n| n as u16))
        })
        .unwrap_or(22);
    let user = {
        let u = get("user_name");
        if u.is_empty() {
            get("username")
        } else {
            u
        }
    };
    draft("fs", name, host, port, user, None)
}

/// 扫描目录下全部 .json 导入
pub async fn import_finalshell_dir(
    store: &Store,
    dir: &std::path::Path,
) -> Result<ExtImportOutcome, StoreError> {
    let mut recs = Vec::new();
    let mut skipped = 0usize;
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            return Err(StoreError::Corrupt(format!(
                "读取 {} 失败: {e}",
                dir.display()
            )))
        }
    };
    for entry in entries.filter_map(Result::ok) {
        let p = entry.path();
        if p.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        {
            match std::fs::read_to_string(&p)
                .ok()
                .and_then(|t| parse_finalshell(&t))
            {
                Some(r) => recs.push(r),
                None => skipped += 1,
            }
        }
    }
    upsert_all(store, "finalshell", recs, skipped).await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_xsh_basic() {
        let text = "[Session]\nName=生产DB\n[CONNECTION]\nHost=10.1.2.3\nPort=2222\n[CONNECTION:AUTHENTICATION]\nUserName=ops\nMethod=0\n";
        let r = parse_xsh(text).expect("parse");
        assert_eq!(r.host, "10.1.2.3");
        assert_eq!(r.port, 2222);
        assert_eq!(r.user, "ops");
        assert_eq!(r.name, "生产DB");
        assert_eq!(r.auth_type, AuthType::Password);
    }

    #[test]
    fn parse_xsh_key_becomes_publickey() {
        let text = "[CONNECTION]\nHost=h1\n[CONNECTION:AUTHENTICATION]\nUserName=u1\nPublicKeyFile=C:\\keys\\id.ppk\n";
        let r = parse_xsh(text).expect("parse");
        assert_eq!(r.auth_type, AuthType::PublicKey);
    }

    #[test]
    fn parse_finalshell_basic() {
        let text = r#"{"name":"线上A","host":"1.2.3.4","port":"22","user_name":"root"}"#;
        let r = parse_finalshell(text).expect("parse");
        assert_eq!(r.host, "1.2.3.4");
        assert_eq!(r.user, "root");
    }

    #[test]
    fn url_decode_spaces() {
        assert_eq!(url_decode("prod%20web%2D01"), "prod web-01");
    }
}
