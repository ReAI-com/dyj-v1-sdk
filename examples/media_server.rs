//! 简易媒体服务器 —— SDK 配套的自部署上传/列表/下载服务（单文件，零外部服务）。
//!
//! 与设备端 upload 域、SDK `ServerClient` 同一契约（卡片 dyj-hd-v2 uploadServer 同构）：
//! - `POST /api/upload/{device_id}/{filename}` — 设备上传（裸二进制；头 `x-filename` /
//!   `x-filesize` / `x-device-id`，可选 `Authorization: Bearer`）→ `{"ok":true,size,sha256}`
//! - `GET /api/files/{device_id}` — 已上传列表 → `{"deviceId","files":[{name,size,modified}]}`
//! - `GET /api/files/{device_id}/{filename}` — 文件回拉（SDK `ServerClient::download_file_to`）
//!
//! 适合开发/联调/私有部署的小体量场景（进程内存放，重启不丢）。生产大体量请换对象存储。
//!
//! 用法：
//!   cargo run --example media_server -- [端口=8900] [数据目录=./dyj-media]
//!   DYJ_MEDIA_TOKEN=secret cargo run --example media_server   # 开启 Bearer 鉴权
//!
//! 设备侧指向本服务器：`dyj_cli upload set http://<本机IP>:<端口>/api/upload`
//! 客户端侧对账：`dyj_cli server files http://<本机IP>:<端口> <deviceId>`

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// 上传单文件上限（与卡片/设备端一致：16MB）。
const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone)]
struct MediaState {
    root: PathBuf,
    token: Option<String>,
}

#[tokio::main]
async fn main() {
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().and_then(|s| s.parse().ok()).unwrap_or(8900);
    let root = PathBuf::from(
        args.next()
            .or_else(|| std::env::var("DYJ_MEDIA_DIR").ok())
            .unwrap_or_else(|| "./dyj-media".into()),
    );
    tokio::fs::create_dir_all(&root)
        .await
        .expect("create data dir");
    let token = std::env::var("DYJ_MEDIA_TOKEN")
        .ok()
        .filter(|s| !s.is_empty());
    let state = MediaState { root, token };

    let app = Router::new()
        .route("/api/upload/{device_id}/{filename}", post(upload))
        .route("/api/files/{device_id}", get(list_files))
        .route("/api/files/{device_id}/{filename}", get(download))
        .with_state(state);

    let addr = format!("0.0.0.0:{port}");
    tracing::info!(target: "dyj.media", "🌐 媒体服务器就绪 http://{addr}");
    tracing::info!(target: "dyj.media", "设备侧: dyj_cli upload set http://<本机IP>:{port}/api/upload");
    tracing::info!(target: "dyj.media", "客户端: dyj_cli server files http://<本机IP>:{port} <deviceId>");
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
    axum::serve(listener, app).await.expect("serve");
}

/// 设备上传（裸二进制流；校验 → 落盘 → sha256 回执）。
async fn upload(
    State(state): State<MediaState>,
    Path((device_id, filename)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    if let Some(expected) = &state.token {
        let ok = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.eq_ignore_ascii_case(&format!("Bearer {expected}")))
            .unwrap_or(false);
        if !ok {
            return Err(unauthorized());
        }
    }
    // 名字防穿越 + 白名单字符（与设备端上传白名单同等保守）
    if !valid_name(&filename) || !valid_id(&device_id) {
        return Err(bad_request("invalid device_id or filename"));
    }
    if body.len() > MAX_FILE_BYTES {
        return Err(bad_request("file too large (16MB cap)"));
    }
    // x-filesize 有则必比（设备端语义：服务端校验 body 长度）
    if let Some(declared) = headers.get("x-filesize").and_then(|v| v.to_str().ok()) {
        if declared.parse::<usize>().ok() != Some(body.len()) {
            return Err(bad_request("x-filesize mismatch"));
        }
    }
    let mut hasher = Sha256::new();
    hasher.update(&body);
    let sha = format!("{:x}", hasher.finalize());

    let dir = state.root.join(&device_id);
    tokio::fs::create_dir_all(&dir).await.map_err(io_err)?;
    tokio::fs::write(dir.join(&filename), &body)
        .await
        .map_err(io_err)?;
    tracing::info!(target: "dyj.media", "⬆ 上传 {device_id}/{filename} {}B", body.len());
    Ok(Json(
        json!({ "ok": true, "size": body.len(), "sha256": sha }),
    ))
}

/// 已上传列表（SDK `ServerClient::list_files` 契约）。
async fn list_files(
    State(state): State<MediaState>,
    Path(device_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    if !valid_id(&device_id) {
        return Err(bad_request("invalid device_id"));
    }
    let mut files = Vec::new();
    let mut rd = tokio::fs::read_dir(state.root.join(&device_id))
        .await
        .map_err(io_err)?;
    while let Some(entry) = rd.next_entry().await.map_err(io_err)? {
        if !entry.file_type().await.map_err(io_err)?.is_file() {
            continue;
        }
        let meta = entry.metadata().await.map_err(io_err)?;
        files.push(json!({
            "name": entry.file_name().to_string_lossy(),
            "size": meta.len(),
            "modified": iso8601(meta.modified().unwrap_or(UNIX_EPOCH)),
        }));
    }
    files.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(Json(json!({ "deviceId": device_id, "files": files })))
}

/// 文件回拉（SDK `ServerClient::download_file_to` 契约）。
async fn download(
    State(state): State<MediaState>,
    Path((device_id, filename)): Path<(String, String)>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    if !valid_name(&filename) || !valid_id(&device_id) {
        return Err(bad_request("invalid device_id or filename"));
    }
    let bytes = tokio::fs::read(state.root.join(device_id).join(&filename))
        .await
        .map_err(|_| not_found())?;
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
        bytes,
    ))
}

// ---- 校验与辅助 ----

/// 名字白名单：字母/数字/`_-.`，拒绝路径分隔与 `..`（与设备端上传白名单同等保守）。
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name != ".."
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// device_id：16 位大写 hex（设备身份），宽松到通用 hex 串。
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.chars().all(|c| c.is_ascii_hexdigit())
}

/// SystemTime → `YYYY-MM-DDTHH:MM:SSZ`（UTC，零依赖：civil 历法反推）。
fn iso8601(t: SystemTime) -> String {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, rem % 3600 / 60, rem % 60);
    // Howard Hinnant civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{y:04}-{mth:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

fn bad_request(msg: &str) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "ok": false, "error": msg })),
    )
}

fn unauthorized() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "ok": false, "error": "bearer token required/invalid" })),
    )
}

fn not_found() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "ok": false, "error": "not found" })),
    )
}

fn io_err(e: std::io::Error) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "ok": false, "error": e.to_string() })),
    )
}
