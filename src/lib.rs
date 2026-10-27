//! reai-dyj-sdk — 点一机 1 代对外接入 SDK。
//!
//! 协议契约见 `PROTOCOL.md`（reai-dyj-sdk 发布仓，单一事实源）。分层：
//! - [`protocol`]: 纯协议层（帧编解码，无线程无 I/O）
//! - [`transport`]: btleplug BLE 传输
//! - [`device`]: 高层设备 API（`DeviceManager` → `DyjDevice`）
//! - [`audio`]: 实时音频（ADPCM 解码 / PCM 块 / WAV 导出）
//! - [`access_point`]: AP 快传 HTTP 客户端（热点模式文件服务）

pub mod access_point;
pub mod audio;
pub mod device;
pub mod error;
pub mod protocol;
pub mod transport;

pub use access_point::{ApClient, ApFileEntry, ApFileList, SavedDownload};
pub use audio::{AmrWbDecoder, PcmChunk};
pub use device::{DeviceEvent, DeviceManager, DyjDevice, RealtimeAudioSession};
pub use error::SdkError;
pub use protocol::{Frame, ProtocolInfo};
