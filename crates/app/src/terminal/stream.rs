//! 终端流热循环 module（卡 2）：8ms/256KB 聚合、信用闸读取、后台 ring 接管与回放。
//! 协议语义（帧格式/ACK/断代）在 stream_protocol.rs；本 module 只管数据通路调度。
//! ByteSource/ByteSink 两条 seam：生产 adapter = russh PTY / ConPTY / Tauri Channel，
//! 测试注入脚本化 fake（慢滴窗口/信用耗尽/前后台切换的确定性单测）。

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use parking_lot::Mutex;
use tauri::ipc::{Channel, Response};

use super::session::{AnyWriter, MacroPending, SuWatch};
use crate::stream_protocol::CreditState;

/// 字节源 seam：read_loop 的唯一输入面（SSH 通道 / ConPTY / 测试 fake）
pub(super) trait ByteSource {
    fn next_data(&mut self) -> impl Future<Output = Option<Bytes>> + Send + '_;
}

/// 字节汇 seam：组帧后的唯一输出口（Tauri 数据 Channel / 测试帧收集器）
pub(super) trait ByteSink {
    /// 送出一帧；false = 对端已关闭（调用方按 C4 断代处理）
    fn send_frame(&self, frame: Vec<u8>) -> bool;
}

impl ByteSink for Channel<Response> {
    fn send_frame(&self, frame: Vec<u8>) -> bool {
        self.send(Response::new(frame)).is_ok()
    }
}

/// 输出聚合时间窗（规格书第 2 条）
const AGG_WINDOW: Duration = Duration::from_millis(8);
/// 单次推送上限（规格书第 2 条）
const AGG_CAP: usize = 256 * 1024;

/// 后台 tab 环形缓冲上限（PR-17 二期资源表 term.ring.cap 行）
pub(super) const RING_CAP: usize = 1024 * 1024;
/// 后台 tab 聚合窗降权（PR-17 二期：前台 8ms → 后台 500ms，先降权再截断）
const BG_AGG_WINDOW: Duration = Duration::from_millis(500);
/// ring 回放单块上限
const RING_DRAIN_CHUNK: usize = 64 * 1024;

/// 后台 tab ring buffer（PR-17 二期 / C11：禁止静默截断字节流）：
/// 覆盖最旧字节必计 truncated；回前台时先注入提示行再按序回放。
pub(super) struct RingBuf {
    pub(super) buf: Mutex<std::collections::VecDeque<u8>>,
    pub(super) cap: usize,
    /// 累计被覆盖丢弃的字节数（回放时取出归零并注入提示）
    pub(super) truncated: AtomicU64,
}

impl RingBuf {
    pub(super) fn new(cap: usize) -> Self {
        Self {
            buf: Mutex::new(std::collections::VecDeque::new()),
            cap,
            truncated: AtomicU64::new(0),
        }
    }

    pub(super) fn push(&self, data: &[u8]) {
        // 单批超 cap：只留尾部（被丢弃的批头计入 truncated——C11 无静默丢弃）
        let data = if data.len() > self.cap {
            let drop = data.len() - self.cap;
            self.truncated.fetch_add(drop as u64, Ordering::Relaxed);
            &data[drop..]
        } else {
            data
        };
        let mut b = self.buf.lock();
        let overflow = (b.len() + data.len()).saturating_sub(self.cap);
        if overflow > 0 {
            let evict = overflow.min(b.len());
            b.drain(..evict);
            self.truncated.fetch_add(evict as u64, Ordering::Relaxed);
        }
        b.extend(data.iter().copied());
    }

    pub(super) fn pop_chunk(&self, n: usize) -> Vec<u8> {
        let mut b = self.buf.lock();
        let k = n.min(b.len());
        b.drain(..k).collect()
    }

    pub(super) fn take_truncated(&self) -> u64 {
        self.truncated.swap(0, Ordering::Relaxed)
    }

    pub(super) fn len(&self) -> usize {
        self.buf.lock().len()
    }
}

/// 终端读取循环：8ms/256KB 聚合 + 信用背压（spike 验证形态）。
/// 信用耗尽即停止 next_data() → russh 不再确认窗口 → 服务端停发，内存不堆积。
/// EOF/Close 时冲净残余即返回——断线语义与重连由 supervise() 负责。
// 参数仅内部装配，非公开 API；clippy 参数数误伤豁免
#[allow(clippy::too_many_arguments)]
pub(super) async fn read_loop<R: ByteSource, K: ByteSink + Sync>(
    reader: &mut R,
    sink: &K,
    credit: &Arc<CreditState>,
    focused: &Arc<AtomicBool>,
    ring: &Arc<RingBuf>,
    out_encoding: Option<&'static encoding_rs::Encoding>,
    // su 二级登录：一次性密码 expect（Some = 本轮 shell 武装中）
    mut su_watch: Option<&mut SuWatch>,
    // 登录宏：su 密码应答成功后一次性下发（Some = 待发；owned——spawn 需 'static）
    macro_pending: &mut Option<MacroPending>,
    // su 应答/宏下发的写半：su_watch 或 macro_pending 为 Some 时必为 Some（supervise 装配不变量）；
    // None 仅存在于测试 fake 装配
    writer: Option<&Arc<AnyWriter>>,
) {
    let mut agg: Vec<u8> = Vec::with_capacity(AGG_CAP);
    // 降权（PR-17 二期）：后台 tab 聚合窗 8ms → 500ms
    let window = |f: &AtomicBool| {
        if f.load(Ordering::Acquire) {
            AGG_WINDOW
        } else {
            BG_AGG_WINDOW
        }
    };
    let mut flush_at = Instant::now() + window(focused);
    // 非 utf-8：流式 Decoder（跨帧半字符内部缓冲），decode 产物进既有聚合通路；
    // utf-8（None）：完全直通，不引入任何拷贝
    let mut decoder = out_encoding.map(crate::encoding::OutputDecoder::new);

    loop {
        // 回前台：先回放 ring（截断提示先行），再处理新输出——顺序即时间序
        if focused.load(Ordering::Acquire) && ring.len() > 0 {
            drain_ring(sink, credit, focused, ring).await;
        }
        let delay = tokio::time::sleep_until(tokio::time::Instant::from_std(flush_at));
        tokio::pin!(delay);
        tokio::select! {
            msg = reader.next_data() => {
                match msg {
                    Some(bytes) => {
                        // su 密码应答：武装窗口内首个密码提示自动答一次；答错不重试
                        // （防锁定），超窗/已答后 pure 旁路——零常态开销
                        if let Some(w) = su_watch.as_mut() {
                            if !w.answered && Instant::now() < w.deadline {
                                if SuWatch::is_password_prompt(&bytes) {
                                    w.answered = true;
                                    if let (Some(pw), Some(wr)) = (&w.password, writer) {
                                        let mut buf = pw.as_bytes().to_vec();
                                        buf.push(b'\r');
                                        let _ = wr.write(&buf).await;
                                    }
                                    // 登录宏：su 应答成功即触发（spawn 不阻塞读循环；
                                    // 密码答错时宏会落入原用户 shell——与手输等价，已知语义）
                                    if let (Some(m), Some(wr)) = (macro_pending.take(), writer) {
                                        let w2 = wr.clone();
                                        tauri::async_runtime::spawn(async move {
                                            super::session::send_macro(&m.lines, &w2, m.input_enc.as_ref()).await;
                                        });
                                    }
                                }
                            } else if Instant::now() >= w.deadline {
                                su_watch = None; // 超窗解除武装
                            }
                        }
                        match decoder.as_mut() {
                            Some(dec) => dec.decode_append(&bytes, &mut agg),
                            None => agg.extend_from_slice(&bytes),
                        }
                        if agg.len() >= AGG_CAP {
                            flush(sink, &mut agg, credit, focused, ring).await;
                            flush_at = Instant::now() + window(focused);
                        }
                    }
                    None => {
                        flush(sink, &mut agg, credit, focused, ring).await;
                        return;
                    }
                }
            }
            _ = &mut delay => {
                if !agg.is_empty() {
                    flush(sink, &mut agg, credit, focused, ring).await;
                }
                flush_at = Instant::now() + window(focused);
            }
        }
    }
}

/// 组帧发送（信用已由调用方取得；C4：alloc 登记先于 send，失败断代不回滚）
fn send_frame(sink: &impl ByteSink, credit: &Arc<CreditState>, buf: Vec<u8>) {
    let Some(frame) = credit.alloc_frame(buf) else {
        return; // offset 溢出断代（信用已耗，随代际销毁）
    };
    if !sink.send_frame(frame) {
        // send 失败：该 epoch 断代——不回滚、不复用 offset、不回补信用
        credit.mark_broken();
        tracing::warn!("终端数据帧发送失败，本 streamEpoch 断代");
    }
}

async fn flush(
    sink: &impl ByteSink,
    agg: &mut Vec<u8>,
    credit: &Arc<CreditState>,
    focused: &Arc<AtomicBool>,
    ring: &Arc<RingBuf>,
) {
    if agg.is_empty() {
        return;
    }
    let buf = std::mem::replace(agg, Vec::with_capacity(AGG_CAP));
    // 断代（send 已失败，前端不可达）：数据直接丢弃，不再消耗信用
    if credit.is_broken() {
        return;
    }
    if focused.load(Ordering::Acquire) {
        // 前台：等待前端信用——背压点；等待期间读取循环挂起
        if !credit.acquire_credit(buf.len() as u32).await {
            return; // 信号量关闭（会话拆除）：丢弃残余数据
        }
    } else {
        // 后台 tab（PR-17 二期）：有信用直发；耗尽不等待（不反压远端进程），
        // 转 ring buffer——覆盖最旧字节必计 truncated，回前台提示并回放（C11）
        if !credit.try_acquire_credit(buf.len() as u32) {
            ring.push(&buf);
            return;
        }
    }
    send_frame(sink, credit, buf);
}

/// 回前台回放（C11）：先注入截断提示行（如有），再按序回放 ring；
/// 走正常信用闸 + 帧分配，offset 语义与常态输出一致。
/// 中途再次退到后台即停（剩余留 ring 等下次前台）。
async fn drain_ring(
    sink: &impl ByteSink,
    credit: &Arc<CreditState>,
    focused: &Arc<AtomicBool>,
    ring: &Arc<RingBuf>,
) {
    let truncated = ring.take_truncated();
    if truncated > 0 {
        let notice = format!(
            "\r\n\x1b[1;33m[myssh] 后台输出已截断 {truncated} 字节（过载保护） \
             / background output truncated {truncated} bytes (overload protection)\x1b[0m\r\n"
        );
        if credit.acquire_credit(notice.len() as u32).await {
            send_frame(sink, credit, notice.into_bytes());
        }
    }
    while focused.load(Ordering::Acquire) {
        let chunk = ring.pop_chunk(RING_DRAIN_CHUNK);
        if chunk.is_empty() {
            break;
        }
        if !credit.acquire_credit(chunk.len() as u32).await {
            return; // 会话拆除
        }
        send_frame(sink, credit, chunk);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::collections::VecDeque;

    use super::*;
    use crate::stream_protocol::{CREDIT_HIGH, FRAME_HEADER_LEN};

    #[test]
    fn ring_buf_fifo_and_cap_eviction_counts_truncated() {
        let r = RingBuf::new(8);
        r.push(b"abcd");
        r.push(b"efgh");
        assert_eq!(r.len(), 8);
        // 溢出 2 字节：覆盖最旧 "ab"，截断计数 2；buffer = "cdefghij"
        r.push(b"ij");
        assert_eq!(r.len(), 8);
        assert_eq!(r.pop_chunk(3), b"cde".to_vec());
        assert_eq!(r.pop_chunk(64), b"fghij".to_vec());
        assert_eq!(r.take_truncated(), 2);
        assert_eq!(r.take_truncated(), 0, "取出即归零");
    }

    #[test]
    fn ring_buf_push_larger_than_cap_keeps_tail() {
        let r = RingBuf::new(4);
        r.push(b"012345");
        assert_eq!(r.pop_chunk(64), b"2345".to_vec());
        assert_eq!(r.take_truncated(), 2);
    }

    // ---- ByteSource/ByteSink fake：脚本化字节源 + 帧收集器（热循环确定性单测）----

    /// 脚本耗尽后挂起（不再产数据也不 EOF），测试结束由 spawn handle abort
    struct FakeSource {
        script: VecDeque<Option<Bytes>>,
    }

    impl FakeSource {
        fn new(script: Vec<Option<Bytes>>) -> Self {
            Self {
                script: script.into(),
            }
        }
    }

    impl ByteSource for FakeSource {
        async fn next_data(&mut self) -> Option<Bytes> {
            match self.script.pop_front() {
                Some(item) => item,
                None => std::future::pending().await,
            }
        }
    }

    #[derive(Default)]
    struct FakeSink {
        frames: Mutex<Vec<Vec<u8>>>,
    }

    impl FakeSink {
        fn payloads(&self) -> Vec<Vec<u8>> {
            self.frames
                .lock()
                .iter()
                .map(|f| f[FRAME_HEADER_LEN..].to_vec())
                .collect()
        }
    }

    impl ByteSink for FakeSink {
        fn send_frame(&self, frame: Vec<u8>) -> bool {
            self.frames.lock().push(frame);
            true
        }
    }

    impl ByteSink for Arc<FakeSink> {
        fn send_frame(&self, frame: Vec<u8>) -> bool {
            (**self).send_frame(frame)
        }
    }

    struct Harness {
        sink: Arc<FakeSink>,
        credit: Arc<CreditState>,
        focused: Arc<AtomicBool>,
        ring: Arc<RingBuf>,
        task: tokio::task::JoinHandle<()>,
    }

    fn spawn_loop(script: Vec<Option<Bytes>>, focused: bool) -> Harness {
        let mut src = FakeSource::new(script);
        let sink = Arc::new(FakeSink::default());
        let credit = Arc::new(CreditState::new(7));
        let focused = Arc::new(AtomicBool::new(focused));
        let ring = Arc::new(RingBuf::new(4096));
        let h = {
            let sink = sink.clone();
            let credit = credit.clone();
            let focused = focused.clone();
            let ring = ring.clone();
            tokio::spawn(async move {
                read_loop(
                    &mut src, &sink, &credit, &focused, &ring, None, None, &mut None, None,
                )
                .await;
            })
        };
        Harness {
            sink,
            credit,
            focused,
            ring,
            task: h,
        }
    }

    /// 暂停时钟下驱动任务：advance 后多次让权，直到 select/锁/信号量链安定
    async fn settle() {
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    }

    /// 慢滴：8ms 聚合窗未满不发帧，越窗才发（暂停时钟确定性）
    #[tokio::test(start_paused = true)]
    async fn agg_window_batches_slow_drip() {
        let h = spawn_loop(vec![Some(Bytes::from_static(b"ab"))], true);
        settle().await; // 消费 chunk 进 agg，注册 8ms 定时器
        tokio::time::advance(Duration::from_millis(7)).await;
        settle().await;
        assert!(h.sink.payloads().is_empty(), "窗口未满不得发帧");
        tokio::time::advance(Duration::from_millis(5)).await; // 越过 8ms 窗
        settle().await;
        assert_eq!(h.sink.payloads(), vec![b"ab".to_vec()]);
        h.task.abort();
    }

    /// 信用耗尽：前台 flush 阻塞在信用闸，ACK 回补后续发（mid-stream 不丢帧、不越闸）
    #[tokio::test(start_paused = true)]
    async fn credit_exhaustion_blocks_then_ack_recovers() {
        let big = vec![0u8; CREDIT_HIGH as usize]; // 恰好耗尽 8MB 信用
        let h = spawn_loop(
            vec![Some(Bytes::from(big)), Some(Bytes::from_static(b"x"))],
            true,
        );
        settle().await; // 大 chunk 触发满帧直发（agg >= AGG_CAP）
        tokio::time::advance(Duration::from_millis(20)).await; // 越过 8ms 窗
        settle().await;
        assert_eq!(h.sink.payloads().len(), 1, "信用耗尽：第二帧不得发出");
        // 前端累计 ACK 回补全部信用 → 阻塞的 flush 放行
        assert_eq!(h.credit.ack(7, CREDIT_HIGH as u64), CREDIT_HIGH as u64);
        settle().await;
        let payloads = h.sink.payloads();
        assert_eq!(payloads.len(), 2, "ACK 后阻塞帧必须补发");
        assert_eq!(payloads[1], b"x".to_vec());
        h.task.abort();
    }

    /// 前后台切换：后台信用耗尽转 ring（不反压远端）；回前台经信用闸按序回放
    #[tokio::test(start_paused = true)]
    async fn background_overflow_rings_and_replays_on_focus() {
        let big = vec![0u8; CREDIT_HIGH as usize];
        let h = spawn_loop(
            vec![Some(Bytes::from(big)), Some(Bytes::from_static(b"bg"))],
            false, // 全程后台起步（500ms 降权窗）
        );
        settle().await; // 大 chunk 直发耗尽信用
        tokio::time::advance(Duration::from_millis(600)).await; // 越过后台 500ms 窗
        settle().await;
        assert_eq!(h.sink.payloads().len(), 1, "后台无信用：不等待，转 ring");
        assert_eq!(h.ring.len(), 2, "溢出字节进 ring");
        // 回前台 + ACK 回补：ring 经正常帧路径回放
        h.focused.store(true, Ordering::Release);
        assert_eq!(h.credit.ack(7, CREDIT_HIGH as u64), CREDIT_HIGH as u64);
        tokio::time::advance(Duration::from_millis(600)).await; // 触发 loop 顶部 drain 检查
        settle().await;
        let payloads = h.sink.payloads();
        assert_eq!(payloads.len(), 2, "回前台必须回放 ring");
        assert_eq!(payloads[1], b"bg".to_vec());
        assert_eq!(h.ring.len(), 0);
        h.task.abort();
    }
}
