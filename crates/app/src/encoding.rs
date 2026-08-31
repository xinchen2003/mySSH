//! 会话级终端编码（encoding_rs 流式转码）。
//!
//! - utf-8：lookup 返回 None，数据通路完全直通（分支在聚合/写入外层，零拷贝）；
//! - 非 utf-8：输出方向 会话字节流 → UTF-8（跨帧半个多字节字符由 Decoder 内部缓冲），
//!   输入方向 前端 UTF-8 → 目标编码。
//! - 转码状态按会话持有：OutputDecoder 随读循环（重连即重建），InputEncoder 随 TermSession。

use encoding_rs::{CoderResult, Decoder, Encoder, Encoding};

/// 编码名（encoding_rs 标签，如 gbk/gb18030/big5/shift_jis/euc-kr）→ 编码；
/// 空/utf-8/未知标签 → None（直通）
pub fn lookup(name: &str) -> Option<&'static Encoding> {
    let n = name.trim();
    if n.is_empty() {
        return None;
    }
    let enc = Encoding::for_label(n.as_bytes())?;
    (enc != encoding_rs::UTF_8).then_some(enc)
}

/// 输出方向：会话字节流 → UTF-8（内部 String 复用，避免逐帧分配）
pub struct OutputDecoder {
    decoder: Decoder,
    buf: String,
}

impl OutputDecoder {
    pub fn new(enc: &'static Encoding) -> Self {
        Self {
            decoder: enc.new_decoder(),
            buf: String::new(),
        }
    }

    /// 流式 decode 一段字节，UTF-8 追加到 out。
    /// last=false：跨帧半字符留在 Decoder 内部，下帧续上；畸形字节替换为 U+FFFD。
    pub fn decode_append(&mut self, src: &[u8], out: &mut Vec<u8>) {
        if src.is_empty() {
            return;
        }
        self.buf.clear();
        // decode_to_string 只写 String 的 spare capacity（不自行增长）：
        // 按最坏情况预留，max 保证一次吃光 src；OutputFull 循环为防御兜底
        let mut rest = src;
        loop {
            if let Some(max) = self.decoder.max_utf8_buffer_length(rest.len()) {
                self.buf.reserve(max);
            }
            let (res, read, _) = self.decoder.decode_to_string(rest, &mut self.buf, false);
            rest = &rest[read..];
            if matches!(res, CoderResult::InputEmpty) || rest.is_empty() {
                break;
            }
        }
        out.extend_from_slice(self.buf.as_bytes());
    }
}

/// 输入方向：前端 UTF-8 → 目标编码
pub struct InputEncoder {
    encoder: Encoder,
}

impl InputEncoder {
    pub fn new(enc: &'static Encoding) -> Self {
        Self {
            encoder: enc.new_encoder(),
        }
    }

    /// 一段 UTF-8 字节 → 目标编码字节。
    /// 前端 TextEncoder 按整串编码，输入必为完整 UTF-8（防御性 lossy）；
    /// 目标编码不可映射的字符走 encoding_rs 默认替换。
    pub fn encode(&mut self, src: &[u8]) -> Vec<u8> {
        let text = String::from_utf8_lossy(src);
        // encode_from_utf8_to_vec 同样只写 Vec 的 spare capacity（不自行增长）
        let mut out = Vec::new();
        let mut rest = text.as_ref();
        loop {
            if let Some(max) = self
                .encoder
                .max_buffer_length_from_utf8_if_no_unmappables(rest.len())
            {
                out.reserve(max);
            }
            // last=true：每次写入是完整消息，编码器收尾（无悬挂状态带入下一条输入）
            let (res, read, _) = self.encoder.encode_from_utf8_to_vec(rest, &mut out, true);
            rest = &rest[read..];
            if matches!(res, CoderResult::InputEmpty) || rest.is_empty() {
                break;
            }
        }
        out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn lookup_utf8_and_unknown_are_passthrough() {
        assert!(lookup("utf-8").is_none());
        assert!(lookup("UTF-8").is_none());
        assert!(lookup("utf8").is_none());
        assert!(lookup("").is_none());
        assert!(lookup("no-such-codec").is_none());
        assert_eq!(lookup("gbk").unwrap(), encoding_rs::GBK);
        assert_eq!(lookup("gb18030").unwrap(), encoding_rs::GB18030);
        assert_eq!(lookup("big5").unwrap(), encoding_rs::BIG5);
        assert_eq!(lookup("shift_jis").unwrap(), encoding_rs::SHIFT_JIS);
        assert_eq!(lookup("euc-kr").unwrap(), encoding_rs::EUC_KR);
    }

    #[test]
    fn output_decoder_buffers_split_multibyte_across_frames() {
        let mut dec = OutputDecoder::new(encoding_rs::GBK);
        // 「中」GBK = 0xD6 0xD0：拆成两帧投喂，第一帧不得产出
        let mut out = Vec::new();
        dec.decode_append(&[0xD6], &mut out);
        assert!(out.is_empty());
        dec.decode_append(&[0xD0], &mut out);
        assert_eq!(String::from_utf8(out).unwrap(), "中");
    }

    #[test]
    fn input_encoder_produces_target_bytes() {
        let mut enc = InputEncoder::new(encoding_rs::GBK);
        assert_eq!(enc.encode("中".as_bytes()), vec![0xD6, 0xD0]);
        // ASCII 逐字节透射
        assert_eq!(enc.encode(b"ls -la\r"), b"ls -la\r");
    }

    #[test]
    fn roundtrip_gbk() {
        let mut enc = InputEncoder::new(encoding_rs::GBK);
        let bytes = enc.encode("你好，世界".as_bytes());
        let mut dec = OutputDecoder::new(encoding_rs::GBK);
        let mut out = Vec::new();
        dec.decode_append(&bytes, &mut out);
        assert_eq!(String::from_utf8(out).unwrap(), "你好，世界");
    }
}
