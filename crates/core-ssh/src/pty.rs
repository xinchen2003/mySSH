//! 交互式 PTY 通道。
//!
//! 读/写分离：app 层由读半驱动 8ms/256KB 聚合循环（读取节奏即背压，规格书第 6 条），
//! 写半独立持有供键盘输入零聚合直发。
//! stderr（ExtendedData）合并进同一数据流——PTY 场景终端本就合并渲染。

use bytes::Bytes;
use russh::client::Msg;
use russh::{ChannelMsg, ChannelReadHalf, ChannelWriteHalf};

use crate::error::SshError;

/// russh 客户端通道的写半携带消息类型参数
type ClientWriteHalf = ChannelWriteHalf<Msg>;

pub struct PtyChannel {
    read: ChannelReadHalf,
    write: ClientWriteHalf,
}

impl PtyChannel {
    pub(crate) fn new(read: ChannelReadHalf, write: ClientWriteHalf) -> Self {
        Self { read, write }
    }

    /// 拆分为读/写两半。读半只能被单一消费循环持有（聚合循环），
    /// 写半可进输入路径任务。
    pub fn split(self) -> (PtyReader, PtyWriter) {
        (PtyReader(self.read), PtyWriter(self.write))
    }
}

/// PTY 读半：逐块产出解密后的数据
pub struct PtyReader(ChannelReadHalf);

impl PtyReader {
    /// 读下一块数据。返回 None 表示对端 EOF/Close 或连接终止。
    /// 窗口调整、请求应答等控制消息在内部跳过。
    pub async fn next_data(&mut self) -> Option<Bytes> {
        loop {
            match self.0.wait().await {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    return Some(data);
                }
                None | Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) => return None,
                Some(_) => continue,
            }
        }
    }
}

/// PTY 写半：键盘输入、resize、关闭
pub struct PtyWriter(ClientWriteHalf);

impl PtyWriter {
    /// 写入用户输入（零聚合直发，规格书输入路径预算）
    pub async fn write(&self, data: &[u8]) -> Result<(), SshError> {
        // 输入路径数据量小，拷贝换 'static，避免借用跨 await
        self.0
            .data_bytes(Bytes::copy_from_slice(data))
            .await
            .map_err(|e| SshError::ChannelIo(e.to_string()))
    }

    /// SIGWINCH：像素维度传 0，由对端按字符维度计算
    pub async fn resize(&self, cols: u32, rows: u32) -> Result<(), SshError> {
        self.0
            .window_change(cols, rows, 0, 0)
            .await
            .map_err(|e| SshError::ChannelIo(e.to_string()))
    }

    pub async fn close(&self) -> Result<(), SshError> {
        self.0
            .close()
            .await
            .map_err(|e| SshError::ChannelIo(e.to_string()))
    }
}
