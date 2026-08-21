//! 判定矩阵抽样测试（M0 验收项：core-policy 的有意义测试）。
#![allow(clippy::unwrap_used)]

use core_policy::{AccessLevel, PolicyEngine, Verdict};

fn eng() -> PolicyEngine {
    PolicyEngine::new()
}

#[test]
fn readonly_allows_whitelisted() {
    assert_eq!(
        eng().classify("ls -la", AccessLevel::Readonly, false),
        Verdict::Allow
    );
    assert_eq!(
        eng().classify(
            "cat /var/log/syslog | grep error | wc -l",
            AccessLevel::Readonly,
            false
        ),
        Verdict::Allow
    );
}

#[test]
fn readonly_denies_unproven() {
    assert_eq!(
        eng().classify("rm file.txt", AccessLevel::Readonly, false),
        Verdict::RequireConfirmation // rm 在全局黑名单 → 确认
    );
    assert_eq!(
        eng().classify("scp a b", AccessLevel::Readonly, false),
        Verdict::Deny // 不在白名单也非黑名单 → readonly 拒绝
    );
}

#[test]
fn standard_requires_confirmation_for_writes() {
    assert_eq!(
        eng().classify("echo hi > /tmp/f", AccessLevel::Standard, false),
        Verdict::RequireConfirmation
    );
    assert_eq!(
        eng().classify("scp a b", AccessLevel::Standard, false),
        Verdict::RequireConfirmation
    );
}

#[test]
fn blacklist_requires_confirmation_at_any_level() {
    for level in [
        AccessLevel::Readonly,
        AccessLevel::Standard,
        AccessLevel::Full,
    ] {
        assert_eq!(
            eng().classify("rm -rf /", level, false),
            Verdict::RequireConfirmation
        );
        assert_eq!(
            eng().classify("dd if=/dev/zero of=/dev/sda", level, false),
            Verdict::RequireConfirmation
        );
    }
}

#[test]
fn compound_evasion_is_caught() {
    // 命令替换穿透
    assert_eq!(
        eng().classify("echo $(rm -rf /)", AccessLevel::Full, false),
        Verdict::RequireConfirmation
    );
    // 反引号穿透
    assert_eq!(
        eng().classify("echo `rm -rf /`", AccessLevel::Full, false),
        Verdict::RequireConfirmation
    );
    // 间接执行 fail-closed
    assert_eq!(
        eng().classify("bash -c 'uptime'", AccessLevel::Full, false),
        Verdict::RequireConfirmation
    );
    // 环境变量前缀绕过
    assert_eq!(
        eng().classify("A=1 rm -rf /", AccessLevel::Full, false),
        Verdict::RequireConfirmation
    );
}

#[test]
fn parse_failure_fails_closed() {
    assert_eq!(
        eng().classify("echo 'unbalanced", AccessLevel::Full, false),
        Verdict::RequireConfirmation
    );
}

#[test]
fn production_forces_confirmation_on_unproven() {
    // production + 全白名单只读 → 仍 Allow
    assert_eq!(
        eng().classify("df -h", AccessLevel::Full, true),
        Verdict::Allow
    );
    // production + 非白名单 → 强制确认（即使 full 档）
    assert_eq!(
        eng().classify("scp a b", AccessLevel::Full, true),
        Verdict::RequireConfirmation
    );
}
