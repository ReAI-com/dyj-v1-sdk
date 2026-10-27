//! SDK 错误类型（语义化快速失败，参考 reai-card-sdk）。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SdkError {
    #[error("bluetooth unavailable: {0}")]
    BluetoothUnavailable(String),

    #[error("device '{0}' not found in scan (is it advertising?)")]
    NotFound(String),

    #[error("command '{command}' timed out after {seconds}s")]
    Timeout { command: String, seconds: u64 },

    #[error("device error: code={code} msg={msg}")]
    Device { code: u32, msg: String },

    #[error("disconnected (command aborted: {command})")]
    Disconnected { command: String },

    #[error("invalid frame: {0}")]
    InvalidFrame(String),

    #[error("invalid params: {0}")]
    InvalidParams(String),

    #[error("ble io: {0}")]
    Ble(String),

    #[error("http: {0}")]
    Http(String),

    #[error("audio consumer too slow: {lost_chunks} chunk(s) dropped; continue on next chunk")]
    AudioLagged { lost_chunks: u64 },
}

pub type Result<T> = std::result::Result<T, SdkError>;
