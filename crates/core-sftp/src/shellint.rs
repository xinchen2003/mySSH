//! Shell 集成（OSC 7 目录上报）rc 文件改写的纯函数。
//!
//! SFTP 面板「目录上报」开关用：enable 往 ~/.bashrc / ~/.zshrc 追加标记块，
//! disable 原样剥离。标记块即状态本身（无需落库）。
//! 走 SFTP 通道写文件，不触碰 shell 输入流——不属于「注入」。

/// 标记块边界（成对出现；手写同标记的块也会被识别，可接受）
pub const MARK_BEGIN: &str = "# >>> myssh osc7 >>>";
pub const MARK_END: &str = "# <<< myssh osc7 <<<";

/// 追加的集成块。bash/zsh 双判定，写进哪个 rc 文件都安全。
/// PROMPT_COMMAND 追加而非覆盖，避免顶掉发行版既有钩子（如 history -a）。
pub const BLOCK: &str = "\
# >>> myssh osc7 >>>
# mySSH 终端目录上报（SFTP「跟随终端目录」数据源）；移除请用 mySSH SFTP 面板开关
if [ -n \"$BASH_VERSION\" ]; then
  __myssh_osc7() { printf '\\033]7;file://%s%s\\007' \"$HOSTNAME\" \"$PWD\"; }
  case \";${PROMPT_COMMAND:-};\" in
    *\";__myssh_osc7;\"*) ;;
    *) PROMPT_COMMAND=\"__myssh_osc7${PROMPT_COMMAND:+;$PROMPT_COMMAND}\" ;;
  esac
elif [ -n \"$ZSH_VERSION\" ]; then
  autoload -Uz add-zsh-hook
  __myssh_osc7() { printf '\\033]7;file://%s%s\\007' \"$HOST\" \"$PWD\"; }
fi
# <<< myssh osc7 <<<";

/// 内容中是否已有集成块
pub fn has_integration(content: &str) -> bool {
    content.contains(MARK_BEGIN)
}

/// 追加集成块（幂等：已有则不重复）
pub fn add_integration(content: &str) -> String {
    if has_integration(content) {
        return content.to_string();
    }
    let mut out = content.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n'); // 与既有内容空行分隔
    }
    out.push_str(BLOCK);
    out.push('\n');
    out
}

/// 剥离集成块（无块则原样返回）。连同块前相邻的一个空行一起清掉。
pub fn remove_integration(content: &str) -> String {
    let mut out: Vec<&str> = Vec::with_capacity(content.lines().count());
    let mut inside = false;
    let mut removed_any = false;
    for line in content.lines() {
        if line.trim() == MARK_BEGIN {
            inside = true;
            removed_any = true;
            // 块前是我们 add 时补的空行 → 一并去掉，避免反复开关留下空行堆积
            if out.last().is_some_and(|l| l.trim().is_empty()) {
                out.pop();
            }
            continue;
        }
        if inside {
            if line.trim() == MARK_END {
                inside = false;
            }
            continue; // 块内行丢弃；块未闭合（用户改坏）则一直剥到文件尾
        }
        out.push(line);
    }
    if !removed_any {
        return content.to_string();
    }
    let mut s = out.join("\n");
    if content.ends_with('\n') && !s.is_empty() {
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_to_empty_creates_block() {
        let out = add_integration("");
        assert!(has_integration(&out));
        assert!(out.contains("PROMPT_COMMAND"));
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn add_is_idempotent() {
        let once = add_integration("existing\n");
        let twice = add_integration(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn add_preserves_existing_content() {
        let out = add_integration("alias ll='ls -l'\n");
        assert!(out.starts_with("alias ll='ls -l'\n"));
        assert!(has_integration(&out));
    }

    #[test]
    fn add_fixes_missing_trailing_newline() {
        let out = add_integration("export A=1");
        assert!(out.contains("export A=1\n"));
        assert!(has_integration(&out));
    }

    #[test]
    fn remove_restores_original() {
        let original = "alias ll='ls -l'\nexport A=1\n";
        let added = add_integration(original);
        assert_eq!(remove_integration(&added), original);
    }

    #[test]
    fn remove_without_block_is_noop() {
        let original = "alias ll='ls -l'\n";
        assert_eq!(remove_integration(original), original);
    }

    #[test]
    fn remove_from_empty_block_file_yields_empty() {
        let added = add_integration("");
        assert_eq!(remove_integration(&added), "");
    }

    #[test]
    fn remove_unclosed_block_strips_to_eof() {
        // 用户手改删了 END 标记：剥到文件尾而非留半截钩子
        let broken = "keep\n\n# >>> myssh osc7 >>>\nPROMPT_COMMAND=xx\n";
        assert_eq!(remove_integration(broken), "keep\n");
    }

    #[test]
    fn block_function_defs_close_with_semicolon() {
        // 回归护栏：bash/zsh 的 { cmd } 要求 } 前有 ; 或换行——54 实测曾因
        // zsh 分支函数缺 ; 导致整个 .bashrc 语法错误、PROMPT_COMMAND 未挂上。
        for line in BLOCK.lines() {
            if line.contains("() {") {
                assert!(line.trim_end().ends_with("; }"), "坏函数定义行: {line}");
            }
        }
        assert!(BLOCK.starts_with(MARK_BEGIN));
        assert!(BLOCK.trim_end().ends_with(MARK_END));
    }
}
