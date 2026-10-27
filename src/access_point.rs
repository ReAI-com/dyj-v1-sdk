//! AP 快传 HTTP 客户端（`ApClient`）：设备热点模式下的文件服务访问
//! （`http://<ap_ip>:8788`，ip 取自 `ap_status`；与 BLE [`crate::device::DyjDevice`] 分离使用）。
//!
//! 端点（协议登记见 PROTOCOL.md §9.1 ap.* 行，形态对齐 dyj-hd-v2 卡片 B）：
//! - `GET /api/files` → `{files:[{name,bytes,mtime}], total}`
//! - `GET /api/files/download?name=<name>` → 流式响应，头部带 `X-Content-MD5`
//!
//! 下载安全：设备端对 name 强校验（录音文件名白名单，拒绝路径分隔符）；客户端发前剥路径
//! 语义只保留纯文件名。下载写 `<dir>/<name>.part`，成功改名、失败清理（dyj-hd-v2 同款）。

use serde::Deserialize;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

use crate::error::{Result, SdkError};

/// `/api/files` 的单个条目。
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct ApFileEntry {
    pub name: String,
    pub bytes: u64,
    pub mtime: u64,
}

/// `GET /api/files` 响应。
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct ApFileList {
    #[serde(default)]
    pub files: Vec<ApFileEntry>,
    #[serde(default)]
    pub total: u64,
}

/// `download_to_dir` 的产物：最终路径 + 校验信息。
#[derive(Debug, Clone)]
pub struct SavedDownload {
    pub path: PathBuf,
    pub name: String,
    pub bytes_written: u64,
    /// 设备端 `X-Content-MD5`；与本地计算不一致时 `download_to_dir` 已直接报错。
    pub md5: Option<String>,
}

/// 设备热点内的 HTTP 文件服务客户端（仅 AP active 时可达）。
#[derive(Clone)]
pub struct ApClient {
    client: reqwest::Client,
    base_url: reqwest::Url,
}

impl ApClient {
    pub fn new(ip: &str, port: u16) -> Result<Self> {
        let base_url = reqwest::Url::parse(&format!("http://{ip}:{port}/"))
            .map_err(|e| SdkError::InvalidFrame(format!("invalid AP endpoint: {e}")))?;
        // 主机没加入设备热点时该 IP 不可达，TCP 建连默认要挂到系统超时（macOS ~75s）；
        // 只限制建连阶段，下载本身不限时
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|e| SdkError::Http(e.to_string()))?;
        tracing::info!(target: "dyj.ap", "🌐 AP 快传客户端就绪 base={base_url}");
        Ok(Self { client, base_url })
    }

    /// 从 `ap_status` 数据取 ip 构造（缺 ip 报错——热点没起来就别连了）。
    pub fn from_status(status: &serde_json::Value) -> Result<Self> {
        let ip = status["ip"]
            .as_str()
            .ok_or_else(|| SdkError::InvalidFrame("ap_status has no ip (AP not active?)".into()))?;
        Self::new(ip, crate::access_point::AP_HTTP_PORT)
    }

    pub fn base_url(&self) -> &reqwest::Url {
        &self.base_url
    }

    /// 列出录音分段（设备端只暴露 files/audio 目录的合法文件）。
    pub async fn list_files(&self) -> Result<ApFileList> {
        let resp = self
            .client
            .get(self.endpoint("api/files")?)
            .send()
            .await
            .map_err(|e| SdkError::Http(e.to_string()))?;
        let resp = ensure_success(resp).await?;
        resp.json::<ApFileList>()
            .await
            .map_err(|e| SdkError::InvalidFrame(format!("bad file list json: {e}")))
    }

    /// 流式下载到 writer（不做落盘与改名；`download_to_dir` 是常规入口）。
    pub async fn download_file_to<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        name: &str,
        writer: &mut W,
    ) -> Result<SavedDownload> {
        let remote = base_name(name);
        tracing::info!(target: "dyj.ap", "🌐 下载 {remote}");
        let resp = self
            .client
            .get(self.endpoint("api/files/download")?)
            .query(&[("name", remote)])
            .send()
            .await
            .map_err(|e| SdkError::Http(e.to_string()))?;
        let mut resp = ensure_success(resp).await?;
        let device_md5 = resp
            .headers()
            .get("x-content-md5")
            .and_then(|v| v.to_str().ok())
            .map(ToOwned::to_owned);

        let mut written = 0u64;
        let mut hasher = md5::Context::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| SdkError::Http(e.to_string()))?
        {
            hasher.consume(&chunk);
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
        let local_md5 = format!("{:x}", hasher.compute());
        if let Some(expected) = &device_md5 {
            if !expected.eq_ignore_ascii_case(&local_md5) {
                return Err(SdkError::InvalidFrame(format!(
                    "md5 mismatch for {remote}: device {expected}, local {local_md5}"
                )));
            }
        }
        tracing::info!(target: "dyj.ap", "✅ 下载完成 {remote} {written}B md5={local_md5}");
        Ok(SavedDownload {
            path: PathBuf::from(remote),
            name: remote.to_owned(),
            bytes_written: written,
            md5: device_md5,
        })
    }

    /// 下载到目录：先写 `<dir>/<name>.part`，成功改名 `<dir>/<name>`，失败清理半截文件。
    pub async fn download_to_dir(
        &self,
        name: &str,
        dir: impl AsRef<Path>,
    ) -> Result<SavedDownload> {
        let dir = dir.as_ref();
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| SdkError::Http(format!("create dir: {e}")))?;
        let original = base_name(name);
        let part = dir.join(format!("{original}.part"));
        let final_path = dir.join(original);
        let mut file = tokio::fs::File::create(&part)
            .await
            .map_err(|e| SdkError::Http(format!("create part: {e}")))?;
        let saved = match self.download_file_to(original, &mut file).await {
            Ok(s) => s,
            Err(e) => {
                drop(file);
                let _ = tokio::fs::remove_file(&part).await; // 失败清理半截文件
                return Err(e);
            }
        };
        drop(file);
        tokio::fs::rename(&part, &final_path)
            .await
            .map_err(|e| SdkError::Http(format!("rename part: {e}")))?;
        Ok(SavedDownload {
            path: final_path,
            ..saved
        })
    }

    /// AP 通道 OTA 上传（wifi.md §4.3，upload-ota 方案 §8）：`POST /api/ota/upload`
    /// 裸二进制 APK + `x-sha256`/`x-size` 头。设备流式落盘并校验后**只暂存不安装**
    /// （返回 `{staged:true,size,sha256}`），安装仍需 BLE [`crate::device::DyjDevice::ota_install`]。
    /// 单包上限 64MB（设备端同款校验，这里前置拦截省一次上传）。
    pub async fn upload_ota(&self, apk_path: impl AsRef<Path>) -> Result<serde_json::Value> {
        const MAX_OTA_UPLOAD_BYTES: u64 = 64 * 1024 * 1024;
        let apk_path = apk_path.as_ref();
        let meta = tokio::fs::metadata(apk_path)
            .await
            .map_err(|e| SdkError::InvalidParams(format!("{}: {e}", apk_path.display())))?;
        if !meta.is_file() {
            return Err(SdkError::InvalidParams(format!(
                "{}: not a file",
                apk_path.display()
            )));
        }
        if meta.len() > MAX_OTA_UPLOAD_BYTES {
            return Err(SdkError::InvalidParams(format!(
                "apk too large: {} > {MAX_OTA_UPLOAD_BYTES} bytes",
                meta.len()
            )));
        }
        let body = tokio::fs::read(apk_path)
            .await
            .map_err(|e| SdkError::Http(format!("read apk: {e}")))?;
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        hasher.update(&body);
        let sha = format!("{:x}", hasher.finalize());
        tracing::info!(
            target: "dyj.ap",
            "🌐 OTA 上传 {} ({}B sha256={}…)", apk_path.display(), body.len(), &sha[..12]
        );
        let resp = self
            .client
            .post(self.endpoint("api/ota/upload")?)
            .header("content-type", "application/octet-stream")
            .header("x-sha256", &sha)
            .header("x-size", body.len())
            .body(body)
            .send()
            .await
            .map_err(|e| SdkError::Http(e.to_string()))?;
        let resp = ensure_success(resp).await?;
        resp.json::<serde_json::Value>()
            .await
            .map_err(|e| SdkError::InvalidFrame(format!("bad ota upload json: {e}")))
    }

    fn endpoint(&self, path: &str) -> Result<reqwest::Url> {
        self.base_url
            .join(path)
            .map_err(|e| SdkError::InvalidFrame(format!("invalid AP path: {e}")))
    }
}

/// 设备端文件服务端口（协议 ap.start 行登记；ApController.HTTP_PORT 同源）。
pub const AP_HTTP_PORT: u16 = 8788;

async fn ensure_success(resp: reqwest::Response) -> Result<reqwest::Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let code = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    Err(SdkError::Device {
        code: u32::from(code),
        msg: body,
    })
}

/// 只取纯文件名（服务端本就拒绝路径分隔符，这里防御性归一）。
fn base_name(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn builds_endpoint_from_status_and_rejects_missing_ip() {
        let c =
            ApClient::from_status(&serde_json::json!({"active":true,"ip":"192.168.43.1"})).unwrap();
        assert_eq!(c.base_url().as_str(), "http://192.168.43.1:8788/");
        assert!(ApClient::from_status(&serde_json::json!({"active":false})).is_err());
    }

    #[tokio::test]
    async fn lists_files_and_parses_entries() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let n = s.read(&mut buf).unwrap();
            let req = std::str::from_utf8(&buf[..n]).unwrap();
            assert!(req.starts_with("GET /api/files HTTP/1.1"), "{req}");
            let body = r#"{"files":[{"name":"audio_20261026-153000_A1B2C3D4_01.amr","bytes":1048576,"mtime":1793000000}],"total":1}"#;
            write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        });
        let list = ApClient::new("127.0.0.1", port)
            .unwrap()
            .list_files()
            .await
            .unwrap();
        assert_eq!(list.total, 1);
        assert_eq!(list.files[0].bytes, 1048576);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn download_to_dir_parts_renames_and_verifies_md5() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let n = s.read(&mut buf).unwrap();
            let req = std::str::from_utf8(&buf[..n]).unwrap();
            assert!(
                req.starts_with("GET /api/files/download?name=audio_x.amr"),
                "{req}"
            );
            let body = b"#!AMR-WB\ntestdata";
            // md5("#!AMR-WB\ntestdata")
            let mut h = md5::Context::new();
            h.consume(body);
            write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nX-Content-MD5: {:x}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", h.compute(), body.len()).unwrap();
            s.write_all(body).unwrap();
        });
        let dir = std::env::temp_dir().join(format!("dyj-ap-test-{}", std::process::id()));

        let saved = ApClient::new("127.0.0.1", port)
            .unwrap()
            .download_to_dir("/audio_x.amr", &dir) // 前导斜杠被剥成纯文件名
            .await
            .unwrap();

        server.join().unwrap();
        assert_eq!(saved.bytes_written, 17);
        assert!(saved.path.ends_with("audio_x.amr"), "{:?}", saved.path);
        assert_eq!(
            tokio::fs::read(&saved.path).await.unwrap(),
            b"#!AMR-WB\ntestdata"
        );
        assert!(!dir.join("audio_x.amr.part").exists(), ".part 应已改名");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn md5_mismatch_cleans_part_file_and_errors() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf).unwrap();
            let body = b"corrupted";
            write!(s, "HTTP/1.1 200 OK\r\nX-Content-MD5: deadbeef\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            s.write_all(body).unwrap();
        });
        let dir = std::env::temp_dir().join(format!("dyj-ap-test-bad-{}", std::process::id()));
        let err = ApClient::new("127.0.0.1", port)
            .unwrap()
            .download_to_dir("audio_y.amr", &dir)
            .await
            .unwrap_err();
        server.join().unwrap();
        assert!(
            matches!(&err, SdkError::InvalidFrame(m) if m.contains("md5 mismatch")),
            "{err}"
        );
        assert!(!dir.join("audio_y.amr.part").exists(), "失败必须清理 .part");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
