//! 终端流协议 module（PR-6 累计 ACK 协议的唯一所有者，卡 1）：
//! 帧格式、代际（streamEpoch）规则、累计 ACK 语义、C4 断代规则。
//!
//! 契约文档：docs/design/03-ipc-contract.md §帧格式/§背压协议；
//! 字节级钉版：fixtures/stream-protocol.jsonl（Rust/TS 两侧测试共用同一语料，漂移即红）。
//! TS 侧对应 adapter：app/ui/src/terminal/stream-protocol.ts（decode/校验）。
//!
//! 不变量：
//! - epoch 随数据 Channel 创建定死（term_open 入参），代际内 sent/acked 单调增；
//! - sent range 登记先于 send（C4）；send 失败即断代——不回滚、不复用 offset、
//!   同 epoch 不再发送，失败批次既不回补信用也不接受其 ACK；
//! - ACK 只接受当前 epoch：`incoming <= acked_total` 丢弃；`incoming > sent_total` 钳制；
//! - 信用 permit 取走即 forget（drop 即归还会使闸门失效——spike 踩坑 #3），
//!   由 acquire_credit/try_acquire_credit 收口，调用方拿不到裸 permit。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::sync::Semaphore;

/// 前端未确认字节数上限（信用背压高水位；超出即停止从 SSH 读取）
pub(crate) const CREDIT_HIGH: u32 = 8 * 1024 * 1024;

/// 帧头长度：streamEpoch | frameSeq | startOffset | endOffset（各 u64 LE，PR-6/C4）
/// frameSeq 为保留字段（写入但无人校验；offset 连续性已覆盖其语义）
pub(crate) const FRAME_HEADER_LEN: usize = 32;

/// 单代际（streamEpoch）终端信用状态：见 module 级不变量。
pub(crate) struct CreditState {
    /// 本代际标识（前端建数据 Channel 时生成，随 term_open 传入）
    pub(crate) epoch: u64,
    /// 已登记发送的字节总量（send 失败也不回滚）
    sent_total: AtomicU64,
    /// 前端已确认消费的字节总量（累计 ACK，单调增）
    acked_total: AtomicU64,
    /// 下一帧序号（单写者：读循环）
    frame_seq: AtomicU64,
    /// send 失败断代标记：本代际不再发送任何数据
    broken: AtomicBool,
    /// 信用闸：可用 permit = 前端还可接收的字节数
    credits: Semaphore,
}

impl CreditState {
    pub(crate) fn new(epoch: u64) -> Self {
        Self {
            epoch,
            sent_total: AtomicU64::new(0),
            acked_total: AtomicU64::new(0),
            frame_seq: AtomicU64::new(0),
            broken: AtomicBool::new(false),
            credits: Semaphore::new(CREDIT_HIGH as usize),
        }
    }

    pub(crate) fn is_broken(&self) -> bool {
        self.broken.load(Ordering::Acquire)
    }

    /// 在途未确认字节数（perf 观测）
    pub(crate) fn outstanding(&self) -> u64 {
        self.sent_total
            .load(Ordering::Acquire)
            .saturating_sub(self.acked_total.load(Ordering::Acquire))
    }

    /// 取走 n 字节信用（前台背压点：等待）。false = 信号量关闭（会话拆除）。
    /// permit forget 语义收口于此：drop 即归还会让闸门失效（spike 踩坑 #3）。
    pub(crate) async fn acquire_credit(&self, n: u32) -> bool {
        match self.credits.acquire_many(n).await {
            Ok(permit) => {
                permit.forget();
                true
            }
            Err(_) => false,
        }
    }

    /// 当前可用信用字节数（perf 观测）
    pub(crate) fn available_credit(&self) -> usize {
        self.credits.available_permits()
    }

    /// 尝试取走 n 字节信用（后台路径：不等待，耗尽转 ring buffer）
    pub(crate) fn try_acquire_credit(&self, n: u32) -> bool {
        match self.credits.try_acquire_many(n) {
            Ok(permit) => {
                permit.forget();
                true
            }
            Err(_) => false,
        }
    }

    /// flush 路径（单写者=读循环）：分配 frameSeq/offset → 登记 sent range → 组帧。
    /// None = 已断代或 offset 溢出（溢出同时断代，不回绕）。
    pub(crate) fn alloc_frame(&self, payload: Vec<u8>) -> Option<Vec<u8>> {
        if self.is_broken() {
            return None;
        }
        let start = self.sent_total.load(Ordering::Acquire);
        let Some(end) = start.checked_add(payload.len() as u64) else {
            // u64 边界（实际不可达：8MB 窗口下需 EB 级输出）：断代处理
            self.broken.store(true, Ordering::Release);
            return None;
        };
        let seq = self.frame_seq.fetch_add(1, Ordering::AcqRel);
        // C4 顺序：登记先于 send——send 失败不回滚
        self.sent_total.store(end, Ordering::Release);
        let mut frame = Vec::with_capacity(FRAME_HEADER_LEN + payload.len());
        frame.extend_from_slice(&self.epoch.to_le_bytes());
        frame.extend_from_slice(&seq.to_le_bytes());
        frame.extend_from_slice(&start.to_le_bytes());
        frame.extend_from_slice(&end.to_le_bytes());
        frame.extend_from_slice(&payload);
        Some(frame)
    }

    /// send 失败：断代（C4：不回滚、不复用 offset、不允许同 epoch 继续发送）
    pub(crate) fn mark_broken(&self) {
        self.broken.store(true, Ordering::Release);
    }

    /// 累计 ACK（term_credit 路径）：返回新增信用字节数；0 = 忽略。
    /// 旧 epoch / 断代 / 重复或倒退 ACK 一律丢弃；incoming > sent 钳制并记协议异常。
    pub(crate) fn ack(&self, epoch: u64, incoming: u64) -> u64 {
        if epoch != self.epoch || self.is_broken() {
            return 0;
        }
        let mut cur = self.acked_total.load(Ordering::Acquire);
        loop {
            if incoming <= cur {
                return 0;
            }
            let sent = self.sent_total.load(Ordering::Acquire);
            if incoming > sent {
                tracing::warn!(epoch, incoming, sent, "终端 ACK 超过已发送量，按钳制处理");
            }
            let newly = (incoming - cur).min(sent.saturating_sub(cur));
            if newly == 0 {
                return 0;
            }
            match self.acked_total.compare_exchange_weak(
                cur,
                cur + newly,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.credits.add_permits(newly as usize);
                    return newly;
                }
                Err(actual) => cur = actual,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn decode_frame(frame: &[u8]) -> (u64, u64, u64, u64, &[u8]) {
        let g = |i: usize| {
            let mut b = [0u8; 8];
            b.copy_from_slice(&frame[i..i + 8]);
            u64::from_le_bytes(b)
        };
        (g(0), g(8), g(16), g(24), &frame[FRAME_HEADER_LEN..])
    }

    /// 消耗 n 字节信用（模拟 flush 前取走）
    fn consume_credit(c: &CreditState, n: u32) {
        assert!(c.try_acquire_credit(n), "信用应充足");
    }

    /// 组帧（应成功；None 即断代/溢出，属本组测试的失败信号）
    fn must_frame(c: &CreditState, payload: Vec<u8>) -> Vec<u8> {
        match c.alloc_frame(payload) {
            Some(f) => f,
            None => panic!("alloc_frame 应成功"),
        }
    }

    #[test]
    fn credit_frame_header_layout_and_register() {
        let c = CreditState::new(7);
        let f0 = must_frame(&c, vec![1, 2, 3]);
        let (epoch, seq, start, end, payload) = decode_frame(&f0);
        assert_eq!((epoch, seq, start, end), (7, 0, 0, 3));
        assert_eq!(payload, &[1, 2, 3]);
        // 第二帧：seq/offset 接续
        let (_, seq1, start1, end1, _) = decode_frame(&must_frame(&c, vec![4]));
        assert_eq!((seq1, start1, end1), (1, 3, 4));
        // sent range 先于 send 登记（C4）
        assert_eq!(c.sent_total.load(Ordering::Relaxed), 4);
        assert_eq!(c.outstanding(), 4);
    }

    #[test]
    fn credit_ack_stale_epoch_dropped() {
        let c = CreditState::new(7);
        consume_credit(&c, 100);
        must_frame(&c, vec![0; 100]);
        // attach 换代后旧 callback 的迟到 ACK（旧 epoch）：无条件丢弃，不回补信用
        assert_eq!(c.ack(8, 100), 0);
        assert_eq!(c.credits.available_permits(), CREDIT_HIGH as usize - 100);
        assert_eq!(c.outstanding(), 100);
    }

    #[test]
    fn credit_ack_cumulative_release() {
        let c = CreditState::new(7);
        consume_credit(&c, 300);
        must_frame(&c, vec![0; 100]);
        must_frame(&c, vec![0; 200]);
        // 累计 ACK：一次确认到 300 → 回补全部 300 permits
        assert_eq!(c.ack(7, 300), 300);
        assert_eq!(c.credits.available_permits(), CREDIT_HIGH as usize);
        assert_eq!(c.outstanding(), 0);
        // 新 epoch（WebView 重载后从零开始）：新代际计数独立，正常放行
        let c2 = CreditState::new(8);
        consume_credit(&c2, 50);
        must_frame(&c2, vec![0; 50]);
        assert_eq!(c2.ack(8, 50), 50);
        assert_eq!(c2.credits.available_permits(), CREDIT_HIGH as usize);
    }

    #[test]
    fn credit_ack_duplicate_or_regressed_ignored() {
        let c = CreditState::new(7);
        consume_credit(&c, 300);
        must_frame(&c, vec![0; 300]);
        assert_eq!(c.ack(7, 200), 200);
        // 同 epoch 重复 ACK / 倒退 ACK：忽略
        assert_eq!(c.ack(7, 200), 0);
        assert_eq!(c.ack(7, 150), 0);
        assert_eq!(c.outstanding(), 100);
        assert_eq!(c.credits.available_permits(), CREDIT_HIGH as usize - 100);
    }

    #[test]
    fn credit_ack_beyond_sent_clamped() {
        let c = CreditState::new(7);
        consume_credit(&c, 100);
        must_frame(&c, vec![0; 100]);
        // ACK 超过已发送量：钳制到 sent_total
        assert_eq!(c.ack(7, 1_000_000), 100);
        assert_eq!(c.credits.available_permits(), CREDIT_HIGH as usize);
    }

    #[test]
    fn credit_send_failure_breaks_epoch() {
        let c = CreditState::new(7);
        consume_credit(&c, 100);
        must_frame(&c, vec![0; 100]);
        c.mark_broken();
        // 断代后不允许同 epoch 继续发送
        assert!(c.alloc_frame(vec![1]).is_none());
        // send 失败批次的迟到 ACK 不得回补信用（该批次未被前端消费）
        assert_eq!(c.ack(7, 100), 0);
        assert_eq!(c.credits.available_permits(), CREDIT_HIGH as usize - 100);
    }

    #[test]
    fn credit_offset_overflow_breaks_epoch() {
        let c = CreditState::new(7);
        c.sent_total.store(u64::MAX - 10, Ordering::Relaxed);
        // u64 边界：不回绕，断代
        assert!(c.alloc_frame(vec![0; 20]).is_none());
        assert!(c.is_broken());
        assert_eq!(c.ack(7, u64::MAX), 0);
    }

    // ---- golden 语料（fixtures/stream-protocol.jsonl，与 TS 侧共用同一份）----

    fn hex_decode(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("合法 hex"))
            .collect()
    }

    fn hex_encode(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// encode/ack 两类用例由 Rust 跑；decode 由 TS 跑（两侧各跳过不拥有的 kind）
    #[test]
    fn golden_corpus() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/stream-protocol.jsonl"
        );
        let text = std::fs::read_to_string(path).expect("读取 golden 语料");
        for line in text.lines() {
            let v: serde_json::Value = serde_json::from_str(line).expect("语料行为合法 JSON");
            match v["kind"].as_str().expect("kind 字段") {
                "encode" => {
                    let name = v["name"].as_str().unwrap();
                    let c = CreditState::new(v["epoch"].as_u64().unwrap());
                    let payloads = v["payloadsHex"].as_array().unwrap();
                    let expects = v["expectHex"].as_array().unwrap();
                    assert_eq!(payloads.len(), expects.len(), "{name}: 数量对齐");
                    for (p, e) in payloads.iter().zip(expects.iter()) {
                        let frame = must_frame(&c, hex_decode(p.as_str().unwrap()));
                        assert_eq!(hex_encode(&frame), e.as_str().unwrap(), "{name}: 字节精确");
                    }
                }
                "ack" => {
                    let name = v["name"].as_str().unwrap();
                    let c = CreditState::new(v["epoch"].as_u64().unwrap());
                    for n in v["send"].as_array().unwrap() {
                        let n = n.as_u64().unwrap() as usize;
                        consume_credit(&c, n as u32);
                        must_frame(&c, vec![0; n]);
                    }
                    for step in v["steps"].as_array().unwrap() {
                        if step.get("break").is_some() {
                            c.mark_broken();
                            continue;
                        }
                        let got = c.ack(
                            step["epoch"].as_u64().unwrap(),
                            step["incoming"].as_u64().unwrap(),
                        );
                        assert_eq!(got, step["newly"].as_u64().unwrap(), "{name}: {step}");
                    }
                }
                _ => {} // meta / decode：非本侧拥有
            }
        }
    }
}
