//! 远端上传服务器客户端（媒体/录音对账与回拉）。
//!
//! 服务端契约（与设备端 upload 域同一服务器，卡片 dyj-hd-v2 uploadServer 同构）：
//! - `GET /api/files/<deviceId>` → `{deviceId, files:[{name, size, modified}]}`
//! - `GET /api/files/<deviceId>/<name>` → 文件流（upload POST 的对称读回路径）
//!
//! base 只传**源站**（如 `https://your-server.example.com`），SDK 派生 `/api/files` 路径；
//! 设备上传 base 若为 `<origin>/api/upload`，则列表/下载与本客户端同源。

use crate::error::{Result, SdkError};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

/// `/api/files/<deviceId>` 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerFileList {
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub files: Vec<ServerFileEntry>,
}

/// 服务器上的一个已上传文件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerFileEntry {
    pub name: String,
    #[serde(default)]
    pub size: u64,
    /// ISO8601 时间字符串（服务器生成，SDK 不解析）。
    #[serde(default)]
    pub modified: String,
}

/// `download_to_dir` 的产物：本地路径 + 字节数。
#[derive(Debug, Clone)]
pub struct SavedServerFile {
    pub path: PathBuf,
    pub name: String,
    pub bytes_written: u64,
}

/// 远端上传服务器客户端（HTTP，与 BLE 连接无关，可独立使用）。
#[derive(Clone)]
pub struct ServerClient {
    client: reqwest::Client,
    origin: reqwest::Url,
}

impl ServerClient {
    /// `origin` = 源站地址（如 `https://your-server.example.com`，末尾 `/` 可省）。
    pub fn new(origin: &str) -> Result<Self> {
        let trimmed = origin.trim_end_matches('/');
        let origin = reqwest::Url::parse(trimmed)
            .map_err(|e| SdkError::InvalidFrame(format!("invalid server origin: {e}")))?;
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| SdkError::Http(e.to_string()))?;
        tracing::info!(target: "dyj.server", "🌐 远端服务器客户端就绪 origin={origin}");
        Ok(Self { client, origin })
    }

    pub fn origin(&self) -> &reqwest::Url {
        &self.origin
    }

    /// 列出该设备已上传到服务器的全部文件（upload 成功删源后的远端侧对账入口）。
    pub async fn list_files(&self, device_id: &str) -> Result<ServerFileList> {
        let url = self
            .origin
            .join(&format!("api/files/{}", encode_seg(device_id)))
            .map_err(|e| SdkError::InvalidFrame(format!("bad files url: {e}")))?;
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| SdkError::Http(e.to_string()))?;
        let resp = ensure_success(resp).await?;
        resp.json::<ServerFileList>()
            .await
            .map_err(|e| SdkError::InvalidFrame(format!("bad server file list json: {e}")))
    }

    /// 流式下载单个文件到 writer（`download_to_dir` 是常规入口）。
    pub async fn download_file_to<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        device_id: &str,
        name: &str,
        writer: &mut W,
    ) -> Result<u64> {
        let remote = base_name(name);
        let url = self
            .origin
            .join(&format!(
                "api/files/{}/{}",
                encode_seg(device_id),
                encode_seg(remote)
            ))
            .map_err(|e| SdkError::InvalidFrame(format!("bad download url: {e}")))?;
        tracing::info!(target: "dyj.server", "🌐 下载 {remote}");
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| SdkError::Http(e.to_string()))?;
        let mut resp = ensure_success(resp).await?;
        let mut written = 0u64;
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| SdkError::Http(e.to_string()))?
        {
            writer
                .write_all(&chunk)
                .await
                .map_err(|e| SdkError::Http(format!("write: {e}")))?;
            written += chunk.len() as u64;
        }
        writer
            .flush()
            .await
            .map_err(|e| SdkError::Http(format!("flush: {e}")))?;
        tracing::info!(target: "dyj.server", "✅ 下载完成 {remote} {written}B");
        Ok(written)
    }

    /// 下载该设备全部（或指定名字子集）文件到目录，返回逐文件结果。
    pub async fn download_to_dir(
        &self,
        device_id: &str,
        dir: impl AsRef<Path>,
        only: Option<&[String]>,
    ) -> Result<Vec<SavedServerFile>> {
        let list = self.list_files(device_id).await?;
        tokio::fs::create_dir_all(dir.as_ref())
            .await
            .map_err(|e| SdkError::Http(format!("mkdir: {e}")))?;
        let mut saved = Vec::new();
        for f in list.files {
            if let Some(names) = only {
                if !names.iter().any(|n| n == &f.name) {
                    continue;
                }
            }
            let path = dir.as_ref().join(&f.name);
            let mut file = tokio::fs::File::create(&path)
                .await
                .map_err(|e| SdkError::Http(format!("create {}: {e}", f.name)))?;
            let bytes = self.download_file_to(device_id, &f.name, &mut file).await?;
            saved.push(SavedServerFile {
                path,
                name: f.name,
                bytes_written: bytes,
            });
        }
        Ok(saved)
    }
}

/// 路径段编码（防注入与非法字符）。
fn encode_seg(seg: &str) -> String {
    reqwest::Url::parse("http://x/").map_or_else(
        |_| seg.to_string(),
        |mut u| {
            u.set_path(seg);
            u.path().trim_start_matches('/').to_string()
        },
    )
}

/// 取文件名部分（与 ApClient 同款防护：客户端给路径只取末段）。
fn base_name(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

async fn ensure_success(resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    Err(SdkError::Http(format!(
        "server {status}: {}",
        body.chars().take(200).collect::<String>()
    )))
}
