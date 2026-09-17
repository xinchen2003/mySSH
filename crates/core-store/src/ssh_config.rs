//! OpenSSH client config（~/.ssh/config）子集解析，供批量导入会话档案。
//!
//! 支持范围（MVP，明确不做 OpenSSH 的参数继承语义）：
//! - Host 块：别名空格分隔，取第一个非通配（不含 `*`/`?`、非 `!` 否定）别名；
//!   全是通配/否定模式的块标记 skipped；`Host *` 全局块整体忽略
//!   （其中 User/IdentityFile 等不合并为其它块的默认值）
//! - 关键字不区分大小写、块内首次出现生效（与 OpenSSH 一致）：
//!   HostName（缺省 = 别名）、Port（缺省 22，非法值按缺省）、User、
//!   IdentityFile（取第一个；`~` 展开为用户目录）、
//!   ProxyJump（取第一跳别名，剥掉 user@ 前缀与 :port 后缀；`none` 忽略）
//! - ProxyCommand 出现即整块标记 skipped（无法映射为 mySSH 跳板链）
//! - 忽略 `#` 注释与空行；支持 `key value` / `key=value` / 引号包裹值

use serde::{Deserialize, Serialize};

/// 一条 Host 块解析结果；skipped 非空 = 不可导入，内容即原因（UI 灰徽标展示）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshConfigEntry {
    pub alias: String,
    pub hostname: String,
    pub port: u16,
    pub user: String,
    pub identity_file: Option<String>,
    pub proxy_jump: Option<String>,
    pub skipped: Option<String>,
}

/// 解析 ssh_config 文本为条目序列（保持文件内顺序）。
/// `home_dir` 用于 IdentityFile 的 `~` 展开；None 时原样保留 `~`。
pub fn parse_ssh_config(text: &str, home_dir: Option<&str>) -> Vec<SshConfigEntry> {
    let mut out = Vec::new();
    let mut cur: Option<Block> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = split_kv(line) else {
            continue;
        };
        if key.eq_ignore_ascii_case("host") {
            if let Some(b) = cur.take() {
                if let Some(e) = b.finish() {
                    out.push(e);
                }
            }
            cur = Some(Block {
                patterns: value.split_whitespace().map(str::to_string).collect(),
                ..Block::default()
            });
            continue;
        }
        // Host 之前的全局指令忽略（见模块文档：不做默认值继承）
        let Some(b) = cur.as_mut() else {
            continue;
        };
        b.apply(&key, &value, home_dir);
    }
    if let Some(b) = cur.take() {
        if let Some(e) = b.finish() {
            out.push(e);
        }
    }
    out
}

#[derive(Default)]
struct Block {
    /// Host 行的全部模式 token（含通配/否定）
    patterns: Vec<String>,
    hostname: Option<String>,
    port: Option<u16>,
    user: Option<String>,
    identity_file: Option<String>,
    proxy_jump: Option<String>,
    has_proxy_command: bool,
}

impl Block {
    fn apply(&mut self, key: &str, value: &str, home_dir: Option<&str>) {
        // OpenSSH 语义：同一关键字块内首次出现生效，重复行忽略
        if key.eq_ignore_ascii_case("hostname") {
            if self.hostname.is_none() {
                self.hostname = Some(value.to_string());
            }
        } else if key.eq_ignore_ascii_case("port") {
            if self.port.is_none() {
                self.port = value.parse().ok();
            }
        } else if key.eq_ignore_ascii_case("user") {
            if self.user.is_none() {
                self.user = Some(value.to_string());
            }
        } else if key.eq_ignore_ascii_case("identityfile") {
            if self.identity_file.is_none() {
                self.identity_file = Some(expand_tilde(value, home_dir));
            }
        } else if key.eq_ignore_ascii_case("proxyjump") {
            if self.proxy_jump.is_none() {
                self.proxy_jump = parse_proxy_jump(value);
            }
        } else if key.eq_ignore_ascii_case("proxycommand") {
            self.has_proxy_command = true;
        }
        // 其余关键字（Compression/ForwardAgent/…）不支持，静默忽略
    }

    fn finish(self) -> Option<SshConfigEntry> {
        // Host * 全局块：整体忽略，不产生条目
        if !self.patterns.is_empty() && self.patterns.iter().all(|p| p == "*") {
            return None;
        }
        let usable = self
            .patterns
            .iter()
            .find(|p| !p.contains(['*', '?']) && !p.starts_with('!'));
        let skipped = match usable {
            None => Some("通配符/否定模式块，无固定别名可导入".to_string()),
            Some(_) if self.has_proxy_command => {
                Some("使用 ProxyCommand（不支持），请手工配置".to_string())
            }
            Some(_) => None,
        };
        let alias = usable
            .cloned()
            .unwrap_or_else(|| self.patterns.first().cloned().unwrap_or_default());
        Some(SshConfigEntry {
            hostname: self.hostname.clone().unwrap_or_else(|| alias.clone()),
            alias,
            port: self.port.unwrap_or(22),
            user: self.user.unwrap_or_default(),
            identity_file: self.identity_file,
            proxy_jump: self.proxy_jump,
            skipped,
        })
    }
}

/// 拆分 `key value` / `key=value` / `key = value`；值去首尾空白与成对引号
fn split_kv(line: &str) -> Option<(String, String)> {
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    if end == 0 {
        return None;
    }
    let key = &line[..end];
    let mut rest = line[end..].trim_start();
    if let Some(r) = rest.strip_prefix('=') {
        rest = r.trim_start();
    }
    if rest.is_empty() {
        return None;
    }
    Some((key.to_string(), strip_quotes(rest)))
}

fn strip_quotes(v: &str) -> String {
    let b = v.as_bytes();
    let quoted = b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[0] == b[b.len() - 1];
    if quoted {
        v[1..v.len() - 1].to_string()
    } else {
        v.to_string()
    }
}

/// ProxyJump：取第一跳；剥掉 user@ 前缀与 :port 后缀，只留跳别名（导入时按会话名解析）
fn parse_proxy_jump(value: &str) -> Option<String> {
    let first = value.split(',').next()?.trim();
    if first.is_empty() || first.eq_ignore_ascii_case("none") {
        return None;
    }
    let host = first.rsplit_once('@').map(|(_, h)| h).unwrap_or(first);
    let host = match host.rsplit_once(':') {
        Some((h, p)) if p.parse::<u16>().is_ok() => h,
        _ => host,
    };
    let host = host.trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// IdentityFile 的 `~` 展开为用户目录；home 未知时原样保留
fn expand_tilde(value: &str, home_dir: Option<&str>) -> String {
    match home_dir {
        Some(h) if value == "~" => h.to_string(),
        Some(h) if value.starts_with("~/") || value.starts_with("~\\") => {
            format!("{h}{}", &value[1..])
        }
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_ssh_config;

    #[test]
    fn multi_alias_takes_first_non_wildcard() {
        let out = parse_ssh_config("Host *.pat web2 web\n  HostName h.example.com\n", None);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].alias, "web2");
        assert_eq!(out[0].hostname, "h.example.com");
        assert!(out[0].skipped.is_none());
    }

    #[test]
    fn wildcard_only_block_marked_skipped() {
        let out = parse_ssh_config("Host *.internal\n  HostName 10.0.0.1\n", None);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].alias, "*.internal");
        assert!(out[0].skipped.is_some());
    }

    #[test]
    fn host_star_global_block_ignored_without_default_merge() {
        let text =
            "Host *\n  User admin\n  IdentityFile ~/.ssh/id_rsa\n\nHost web\n  HostName 1.2.3.4\n";
        let out = parse_ssh_config(text, Some("/home/me"));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].alias, "web");
        // MVP 不做参数继承：全局块的 User/IdentityFile 不得合并进来
        assert_eq!(out[0].user, "");
        assert_eq!(out[0].identity_file, None);
    }

    #[test]
    fn equals_form_comments_and_blank_lines() {
        let text = "# 顶部注释\n\nHost=db\n  HostName=db.internal  \n  # 行内注释\n  Port=2222\n";
        let out = parse_ssh_config(text, None);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].alias, "db");
        assert_eq!(out[0].hostname, "db.internal");
        assert_eq!(out[0].port, 2222);
    }

    #[test]
    fn identity_file_first_wins_and_tilde_expands() {
        let text = "Host web\n  IdentityFile ~/.ssh/id_ed25519\n  IdentityFile ~/.ssh/id_rsa\n";
        let out = parse_ssh_config(text, Some("C:\\Users\\me"));
        assert_eq!(
            out[0].identity_file.as_deref(),
            Some("C:\\Users\\me/.ssh/id_ed25519")
        );
    }

    #[test]
    fn proxy_jump_none_ignored_and_first_hop_taken() {
        let out = parse_ssh_config(
            "Host a\n  ProxyJump none\nHost b\n  ProxyJump user@bastion:2222,hop2\n",
            None,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].proxy_jump, None);
        assert_eq!(out[1].proxy_jump.as_deref(), Some("bastion"));
    }

    #[test]
    fn keywords_case_insensitive_and_quoted_values() {
        let text = "HOST web\n  hostname \"ex ample.com\"\n  USER 'deploy'\n";
        let out = parse_ssh_config(text, None);
        assert_eq!(out[0].hostname, "ex ample.com");
        assert_eq!(out[0].user, "deploy");
    }

    #[test]
    fn proxy_command_marks_skipped() {
        let out = parse_ssh_config(
            "Host web\n  HostName 1.2.3.4\n  ProxyCommand nc %h %p\n",
            None,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].alias, "web");
        assert!(out[0].skipped.is_some());
    }

    #[test]
    fn defaults_hostname_alias_and_port_22() {
        let out = parse_ssh_config("Host mybox\n  User ops\n", None);
        assert_eq!(out[0].hostname, "mybox");
        assert_eq!(out[0].port, 22);
        assert_eq!(out[0].user, "ops");
    }

    #[test]
    fn negated_pattern_not_chosen_as_alias() {
        let out = parse_ssh_config("Host !bastion *.pat real\n  HostName h\n", None);
        assert_eq!(out[0].alias, "real");
    }
}
