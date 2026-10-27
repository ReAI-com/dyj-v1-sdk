//! 纯协议层：文本帧 CMD/RSP/EVT + 二进制帧（协议规则见 PROTOCOL.md）。
//! 无线程、无 I/O，可直接单测。

use serde_json::{json, Value};

pub const FRAME_DELIMITER: &str = "\r\n";

/// 协议版本与能力位（与设备端 core/protocol/builtin/ProtoHandlers.kt 保持一致）。
pub struct ProtocolInfo;

impl ProtocolInfo {
    pub const PROTO_VERSION: u64 = 1;
    pub const FEATURE_HELLO: u64 = 1;
    pub const FEATURE_BINARY_TYPED_FRAMES: u64 = 1 << 1;
    pub const FEATURE_REALTIME_AUDIO: u64 = 1 << 2;
}

/// 文本帧。
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// 客户端 → 设备 请求。id 为响应匹配键。
    Request { id: u64, cmd: String, params: Value },
    /// 设备 → 客户端 响应。
    Response {
        id: u64,
        code: u32,
        msg: String,
        data: Value,
    },
    /// 设备 → 客户端 主动事件。
    Event { evt: String, ts: u64, data: Value },
}

impl Frame {
    pub fn encode(&self) -> String {
        let (prefix, obj) = match self {
            Frame::Request { id, cmd, params } => {
                ("CMD:", json!({ "id": id, "cmd": cmd, "params": params }))
            }
            Frame::Response {
                id,
                code,
                msg,
                data,
            } => (
                "RSP:",
                json!({ "id": id, "code": code, "msg": msg, "data": data }),
            ),
            Frame::Event { evt, ts, data } => {
                ("EVT:", json!({ "evt": evt, "ts": ts, "data": data }))
            }
        };
        format!("{prefix}{}{FRAME_DELIMITER}", obj)
    }

    /// 解析单个已去定界符的帧文本；非法返回 None（调用方丢弃并计数）。
    pub fn decode(line: &str) -> Option<Frame> {
        let (prefix, rest) = line
            .strip_prefix("CMD:")
            .map(|r| ("CMD", r))
            .or_else(|| line.strip_prefix("RSP:").map(|r| ("RSP", r)))
            .or_else(|| line.strip_prefix("EVT:").map(|r| ("EVT", r)))?;
        let v: Value = serde_json::from_str(rest).ok()?;
        match prefix {
            "CMD" => Some(Frame::Request {
                id: v.get("id")?.as_u64()?,
                cmd: v.get("cmd")?.as_str()?.to_string(),
                params: v.get("params").cloned().unwrap_or(Value::Null),
            }),
            "RSP" => Some(Frame::Response {
                id: v.get("id")?.as_u64()?,
                code: v.get("code")?.as_u64()? as u32,
                msg: v
                    .get("msg")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_string(),
                data: v.get("data").cloned().unwrap_or(Value::Null),
            }),
            _ => Some(Frame::Event {
                evt: v.get("evt")?.as_str()?.to_string(),
                ts: v.get("ts").and_then(|t| t.as_u64()).unwrap_or(0),
                data: v.get("data").cloned().unwrap_or(Value::Null),
            }),
        }
    }
}

/// 流式文本帧组装器：BLE write 任意切分 → 按 \r\n 切出完整帧。
pub struct FrameAssembler {
    buffer: String,
    max_buffer: usize,
}

impl Default for FrameAssembler {
    fn default() -> Self {
        Self {
            buffer: String::new(),
            max_buffer: 64 * 1024,
        }
    }
}

impl FrameAssembler {
    /// 喂入一段收到的文本，返回其中包含的完整帧文本（不含定界符）。
    pub fn feed(&mut self, chunk: &str) -> Vec<String> {
        self.buffer.push_str(chunk);
        let mut frames = Vec::new();
        while let Some(idx) = self.buffer.find(FRAME_DELIMITER) {
            let line: String = self.buffer.drain(..idx + FRAME_DELIMITER.len()).collect();
            let line = line.trim_end_matches(FRAME_DELIMITER);
            if !line.is_empty() {
                frames.push(line.to_string());
            }
        }
        if self.buffer.len() > self.max_buffer {
            self.buffer.clear();
        }
        frames
    }

    pub fn reset(&mut self) {
        self.buffer.clear();
    }
}

/// 二进制帧（协议规则 §3.2）：`[type:1][flags:1][seq_le:2][len_le:2][payload]`。
#[derive(Debug, Clone, PartialEq)]
pub struct BinaryFrame {
    pub frame_type: u8,
    pub flags: u8,
    pub seq: u16,
    pub payload: Vec<u8>,
}

impl BinaryFrame {
    pub const HEADER_SIZE: usize = 6;
    pub const FLAG_FRAGMENT_START: u8 = 0x01;
    pub const FLAG_FRAGMENT_END: u8 = 0x02;
    pub const FLAG_NEED_ACK: u8 = 0x04;
    pub const TYPE_LEGACY_AUDIO: u8 = 0x00;
    pub const TYPE_REALTIME_AUDIO: u8 = 0x01;
    pub const TYPE_FILE_BLOCK: u8 = 0x02;
    pub const TYPE_HEARTBEAT: u8 = 0x03;
    /// 实时音频 AMR-WB（16kHz 12.65kbps，20ms 帧批打包；链路实测 ~3.6KB/s 上限）。
    pub const TYPE_REALTIME_AUDIO_AMR: u8 = 0x04;

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::HEADER_SIZE + self.payload.len());
        out.push(self.frame_type);
        out.push(self.flags);
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&(self.payload.len() as u16).to_le_bytes());
        out.extend_from_slice(&self.payload);
        out
    }

    pub fn decode(data: &[u8]) -> Option<BinaryFrame> {
        if data.len() < Self::HEADER_SIZE {
            return None;
        }
        let len = u16::from_le_bytes([data[4], data[5]]) as usize;
        if data.len() - Self::HEADER_SIZE != len {
            return None;
        }
        Some(BinaryFrame {
            frame_type: data[0],
            flags: data[1],
            seq: u16::from_le_bytes([data[2], data[3]]),
            payload: data[Self::HEADER_SIZE..].to_vec(),
        })
    }
}

/// 流式二进制帧组装器：按 6 字节头的 len 字段切出完整帧。
///
/// 契约：每次 `feed` 喂入**一条 BLE 通知**（设备端帧不跨通知，166B < MTU-3）。
/// 头部声明长度超出单帧上限（`MAX_BINARY_PAYLOAD`）视为错位/损坏，整段丢弃——
/// 下一条通知自然恢复，不做字节级重同步（无同步头的长度前缀流无法可靠重同步）。
#[derive(Default)]
pub struct BinaryFrameAssembler {
    buffer: Vec<u8>,
}

/// 单帧载荷上限：MTU 517 时单 notify 最多 514B，留裕量取 1024。
pub const MAX_BINARY_PAYLOAD: usize = 1024;

impl BinaryFrameAssembler {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<BinaryFrame> {
        self.buffer.extend_from_slice(chunk);
        let mut frames = Vec::new();
        loop {
            if self.buffer.len() < BinaryFrame::HEADER_SIZE {
                break;
            }
            let len = u16::from_le_bytes([self.buffer[4], self.buffer[5]]) as usize;
            if len > MAX_BINARY_PAYLOAD {
                // 头部非法 = 本通知已错位，整段丢弃（含后续半帧），等下一条通知
                self.buffer.clear();
                break;
            }
            let total = BinaryFrame::HEADER_SIZE + len;
            if self.buffer.len() < total {
                break;
            }
            let frame_bytes: Vec<u8> = self.buffer.drain(..total).collect();
            if let Some(frame) = BinaryFrame::decode(&frame_bytes) {
                // 未知 type 按协议丢弃并计数（§3.2），不上行
                if frame.frame_type <= BinaryFrame::TYPE_REALTIME_AUDIO_AMR {
                    frames.push(frame);
                }
            }
        }
        frames
    }

    pub fn reset(&mut self) {
        self.buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let f = Frame::Request {
            id: 42,
            cmd: "wifi.list".into(),
            params: json!({"scan": true}),
        };
        let s = f.encode();
        assert!(s.starts_with("CMD:") && s.ends_with("\r\n"));
        assert_eq!(f, Frame::decode(s.trim_end_matches("\r\n")).unwrap());
    }

    #[test]
    fn response_roundtrip() {
        let f = Frame::Response {
            id: 1,
            code: 0,
            msg: "ok".into(),
            data: json!({"message": "pong"}),
        };
        assert_eq!(
            f,
            Frame::decode(f.encode().trim_end_matches("\r\n")).unwrap()
        );
    }

    #[test]
    fn garbage_returns_none() {
        assert!(Frame::decode("hello").is_none());
        assert!(Frame::decode("CMD:{broken").is_none());
        assert!(Frame::decode("CMD:{\"no_cmd\":1}").is_none());
    }

    #[test]
    fn assembler_splits_across_chunks() {
        let mut asm = FrameAssembler::default();
        let s = Frame::Request {
            id: 7,
            cmd: "proto.echo".into(),
            params: json!({"message": "hi"}),
        }
        .encode();
        let mid = s.len() / 2;
        assert!(asm.feed(&s[..mid]).is_empty());
        let out = asm.feed(&s[mid..]);
        assert_eq!(out, vec![s.trim_end_matches("\r\n").to_string()]);
    }

    #[test]
    fn binary_frame_roundtrip() {
        let raw: Vec<u8> = vec![0x01, 0x03, 0x05, 0x00, 0x02, 0x00, 0xAA, 0xBB];
        let f = BinaryFrame::decode(&raw).unwrap();
        assert_eq!(f.frame_type, 0x01);
        assert_eq!(f.seq, 5);
        assert_eq!(f.payload, vec![0xAA, 0xBB]);
        assert!(BinaryFrame::decode(&raw[..5]).is_none());
        assert_eq!(BinaryFrame::decode(&f.encode()).unwrap(), f);
    }

    #[test]
    fn binary_assembler_reassembles_split_frames() {
        let frame = BinaryFrame {
            frame_type: BinaryFrame::TYPE_REALTIME_AUDIO,
            flags: 0,
            seq: 42,
            payload: vec![1; 160],
        };
        let raw = frame.encode();
        let mut asm = BinaryFrameAssembler::default();
        assert!(asm.feed(&raw[..40]).is_empty());
        let out = asm.feed(&raw[40..]);
        assert_eq!(out, vec![frame]);
    }

    #[test]
    fn binary_assembler_skips_garbage_prefix() {
        let frame = BinaryFrame {
            frame_type: BinaryFrame::TYPE_HEARTBEAT,
            flags: 0,
            seq: 0,
            payload: vec![],
        };
        let raw = frame.encode();
        let mut asm = BinaryFrameAssembler::default();
        // 错位通知整段丢弃，下一条通知（完整帧）自然恢复
        let mut garbage = vec![0xFF; 10];
        garbage.extend_from_slice(&raw);
        assert!(asm.feed(&garbage).is_empty());
        assert_eq!(asm.feed(&raw), vec![frame]);
    }

    #[test]
    fn binary_assembler_discards_unknown_type() {
        let mut asm = BinaryFrameAssembler::default();
        let frame = BinaryFrame {
            frame_type: 0x80,
            flags: 0,
            seq: 1,
            payload: vec![9; 4],
        };
        // 前向兼容：新版本设备发的未知 type 丢弃不炸
        assert!(asm.feed(&frame.encode()).is_empty());
        let known = BinaryFrame {
            frame_type: 3,
            flags: 0,
            seq: 2,
            payload: vec![],
        };
        assert_eq!(asm.feed(&known.encode()), vec![known]);
    }

    #[test]
    fn binary_assembler_emits_back_to_back_frames() {
        let mut asm = BinaryFrameAssembler::default();
        let a = BinaryFrame {
            frame_type: 1,
            flags: 0,
            seq: 1,
            payload: vec![2; 10],
        };
        let b = BinaryFrame {
            frame_type: 1,
            flags: 0,
            seq: 2,
            payload: vec![3; 20],
        };
        let mut raw = a.encode();
        raw.extend(b.encode());
        let out = asm.feed(&raw);
        assert_eq!(out, vec![a, b]);
    }
}
