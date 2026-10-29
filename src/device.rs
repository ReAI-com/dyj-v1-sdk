//! 高层设备 API：`DeviceManager::scan/connect` → `DyjDevice`（请求-响应匹配 + 事件流）。

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc as StdArc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

use btleplug::api::Manager as _;

use crate::error::{Result, SdkError};
use crate::protocol::{Frame, ProtocolInfo};
use crate::transport::{self, ConnectedTransport};

/// 设备事件（语义事件 + 响应外的原始帧兜底，前向兼容新固件事件不丢数据）。
#[derive(Debug, Clone)]
pub enum DeviceEvent {
    Event { evt: String, ts: u64, data: Value },
    Disconnected,
}

struct PendingMap(Mutex<HashMap<u64, mpsc::Sender<Frame>>>);

/// 响应是否已收尾（协议 §3.3）：data 无 `more:true` 即终结帧（普通命令天然满足）。
fn response_done(data: &Value) -> bool {
    data.get("more").and_then(Value::as_bool) != Some(true)
}

#[derive(Clone)]
pub struct DyjDevice {
    name: String,
    transport: StdArc<ConnectedTransport>,
    pending: Arc<PendingMap>,
    next_id: Arc<AtomicU64>,
    event_tx: broadcast::Sender<DeviceEvent>,
    audio_tx: Arc<broadcast::Sender<crate::audio::PcmChunk>>,
    _keepalive: Arc<tokio::sync::mpsc::Sender<()>>, // reader 任务持有句柄，drop 即断连
}

impl DyjDevice {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub async fn is_connected(&self) -> Result<bool> {
        self.transport.is_connected().await
    }

    /// 显式断开 BLE 链路（设备端才能恢复广播）。
    pub async fn disconnect(&self) -> Result<()> {
        self.transport.disconnect().await
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<DeviceEvent> {
        self.event_tx.subscribe()
    }

    /// 订阅实时音频流（FF12）：AMR-WB/ADPCM 都在 SDK 内解码为 16kHz 单声道 PCM。
    /// 广播通道：慢消费者丢块（Lagged，`SdkError::AudioLagged`），seq 断流标记
    /// [`PcmChunk::discontinuous`]。16kHz PCM 与 reai-card-sdk 接口形态一致，可直接进
    /// driver-v2 的实时链路（ASR/VAD/播放）。
    pub fn subscribe_audio(&self) -> broadcast::Receiver<crate::audio::PcmChunk> {
        self.audio_tx.subscribe()
    }

    /// 发送命令并等待响应（协议规则 §4：串行、超时返回 1004 对应的 [SdkError::Timeout]）。
    ///
    /// 支持 indexed 流式响应（协议 §3.3）：data 含 `index` 的多条同 id RSP 逐帧到达，
    /// `entry` 累积；`more:false` 收尾帧（带 total）结束后返回 `{total, items:[...]}`。
    /// 普通单帧命令行为不变。流式进行中以空闲超时逐帧续期（大列表总时长不可预知）。
    pub async fn request(&self, cmd: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, mut rx) = mpsc::channel::<Frame>(32);
        self.pending.0.lock().unwrap().insert(id, tx);

        let frame = Frame::Request {
            id,
            cmd: cmd.to_string(),
            params,
        };
        tracing::debug!(target: "dyj.device", cmd = %cmd, id, "request sent");
        if self.transport.write_line(&frame.encode()).await.is_err() {
            self.pending.0.lock().unwrap().remove(&id);
            return Err(SdkError::Disconnected {
                command: cmd.to_string(),
            });
        }

        let t0 = std::time::Instant::now();
        let mut items: Vec<Value> = Vec::new();
        let done = |data: &Value| response_done(data);
        loop {
            // 首帧用剩余总超时；流式后续帧用整段空闲超时（逐帧续期）
            let wait = if items.is_empty() {
                timeout.saturating_sub(t0.elapsed())
            } else {
                timeout
            };
            let reply = match tokio::time::timeout(wait, rx.recv()).await {
                Ok(Some(f)) => f,
                Ok(None) => {
                    self.pending.0.lock().unwrap().remove(&id);
                    return Err(SdkError::Disconnected {
                        command: cmd.to_string(),
                    });
                }
                Err(_) => {
                    self.pending.0.lock().unwrap().remove(&id);
                    return Err(SdkError::Timeout {
                        command: cmd.to_string(),
                        seconds: timeout.as_secs(),
                    });
                }
            };
            let Frame::Response {
                code, msg, data, ..
            } = reply
            else {
                self.pending.0.lock().unwrap().remove(&id);
                return Err(SdkError::InvalidFrame(
                    "unexpected frame type for id".into(),
                ));
            };
            if code != 0 {
                self.pending.0.lock().unwrap().remove(&id);
                tracing::info!(target: "dyj.device", cmd = %cmd, id, code, elapsed_ms = t0.elapsed().as_millis() as u64, "command done");
                return Err(SdkError::Device { code, msg });
            }
            if data.get("index").is_none() {
                self.pending.0.lock().unwrap().remove(&id);
                tracing::info!(target: "dyj.device", cmd = %cmd, id, code, elapsed_ms = t0.elapsed().as_millis() as u64, "command done");
                return Ok(data);
            }
            if let Some(entry) = data.get("entry") {
                items.push(entry.clone());
            }
            if done(&data) {
                self.pending.0.lock().unwrap().remove(&id);
                let total = data
                    .get("total")
                    .and_then(Value::as_u64)
                    .unwrap_or(items.len() as u64);
                tracing::info!(target: "dyj.device", cmd = %cmd, id, items = items.len(), elapsed_ms = t0.elapsed().as_millis() as u64, "streamed command done");
                return Ok(json!({ "total": total, "items": items }));
            }
            // more=true：继续收下一帧
        }
    }

    /// `proto.hello` 握手（L0，5s）。`auto_audio=false` 关闭设备端"连上即有"的自动开流。
    pub async fn hello(&self, client_name: &str, auto_audio: bool) -> Result<Value> {
        self.request(
            "proto.hello",
            json!({
                "name": client_name,
                "feature_bits": ProtocolInfo::FEATURE_HELLO
                    | ProtocolInfo::FEATURE_BINARY_TYPED_FRAMES
                    | ProtocolInfo::FEATURE_REALTIME_AUDIO,
                "auto_audio": auto_audio,
            }),
            Duration::from_secs(5),
        )
        .await
    }

    /// `audio.stream.start` 开启实时音频流（FF12 type=0x01，20ms ADPCM 块）。
    pub async fn audio_stream_start(&self) -> Result<Value> {
        self.request("audio.stream.start", json!({}), Duration::from_secs(5))
            .await
    }

    /// `audio.stream.stop` 关闭实时音频流。
    pub async fn audio_stream_stop(&self) -> Result<Value> {
        self.request("audio.stream.stop", json!({}), Duration::from_secs(5))
            .await
    }

    /// `audio.capture.start` 开启采集引擎（mic + AMR-WB 编码；流/录音的数据源）。
    pub async fn audio_capture_start(&self) -> Result<Value> {
        self.request("audio.capture.start", json!({}), Duration::from_secs(5))
            .await
    }

    /// `audio.capture.stop` 关闭采集引擎（stream/record 开关状态不动）。
    pub async fn audio_capture_stop(&self) -> Result<Value> {
        self.request("audio.capture.stop", json!({}), Duration::from_secs(5))
            .await
    }

    /// `record.start` 开始本地录音（默认 10 分钟分段，卡片逻辑；SDK 侧无需订阅）。
    /// `session`：可选 8 位大写 hex 会话编号（对齐卡片 0.3.8——同一次录音的分段共享
    /// session、seq 递增，云端/服务端按名分组）；缺省设备生成，非法被设备拒绝。
    pub async fn record_start(
        &self,
        segment_min: Option<u64>,
        session: Option<&str>,
    ) -> Result<Value> {
        let mut params = serde_json::Map::new();
        if let Some(m) = segment_min {
            params.insert("segment_min".into(), json!(m));
        }
        if let Some(s) = session {
            params.insert("session".into(), json!(s));
        }
        self.request(
            "record.start",
            Value::Object(params),
            Duration::from_secs(5),
        )
        .await
    }

    /// `record.stop` 停止本地录音。
    pub async fn record_stop(&self) -> Result<Value> {
        self.request("record.stop", json!({}), Duration::from_secs(5))
            .await
    }

    /// `record.list` 录音文件列表（indexed 流式聚合）。
    /// 返回 `{total, items:[{name, bytes, mtime, session?, seq?}]}`，按文件名（时间）升序；
    /// session/seq 从文件名解析（会话命名规则见协议 §9.1），旧命名文件不带。
    pub async fn record_list(&self) -> Result<Value> {
        self.request("record.list", json!({}), Duration::from_secs(10))
            .await
    }

    /// `record.status` 录音状态（文件数/字节/当前分段）。
    pub async fn record_status(&self) -> Result<Value> {
        self.request("record.status", json!({}), Duration::from_secs(5))
            .await
    }

    /// `led.status` 当前灯效与 claim 栈（L1）。
    /// 设备端 indexed 流式（帧 ≤MTU-3，整包响应超 MTU 会被截断丢帧），SDK 聚合并重塑为
    /// `{active, claims, total}`。
    pub async fn led_status(&self) -> Result<Value> {
        let agg = self
            .request_idem("led.status", json!({}), Duration::from_secs(5))
            .await?;
        let mut items = agg["items"].as_array().cloned().unwrap_or_default();
        let meta = if items.is_empty() {
            json!({})
        } else {
            items.remove(0)
        };
        Ok(json!({
            "active": meta.get("active").cloned().unwrap_or(Value::Null),
            "claims": items,
            "total": agg["total"],
        }))
    }

    /// `led.set` 临时设置自定义灯效（默认 priority=850，不持久化）。
    /// `timeout_s` 到期设备自动回落；被更高优先级系统状态压制时仍 accepted，解除后自动生效。
    pub async fn led_set(
        &self,
        pattern: &str,
        brightness: Option<&str>,
        priority: Option<u32>,
        timeout_s: Option<u64>,
    ) -> Result<Value> {
        let mut params = serde_json::Map::new();
        params.insert("pattern".into(), json!(pattern));
        if let Some(b) = brightness {
            params.insert("brightness".into(), json!(b));
        }
        if let Some(p) = priority {
            params.insert("priority".into(), json!(p));
        }
        if let Some(t) = timeout_s {
            params.insert("timeout_s".into(), json!(t));
        }
        self.request_idem("led.set", Value::Object(params), Duration::from_secs(5))
            .await
    }

    /// `led.clear` 清除自定义灯效（ble-override）与故障锁存（fault）并回落。
    pub async fn led_clear(&self) -> Result<Value> {
        self.request_idem("led.clear", json!({}), Duration::from_secs(5))
            .await
    }

    /// `button.status` 按键能力查询（L1）：支持手势/判定阈值（只读）/绑定表/最近事件。
    pub async fn button_status(&self) -> Result<Value> {
        self.request_idem("button.status", json!({}), Duration::from_secs(5))
            .await
    }

    /// `button.bind.get` 查询手势→动作绑定表（L1）。
    /// 返回 `{bindings:{click,double_click,long_press}}`，默认全 `event_only`。
    pub async fn button_bind_get(&self) -> Result<Value> {
        self.request_idem("button.bind.get", json!({}), Duration::from_secs(5))
            .await
    }

    /// `button.bind.set` 设置单个手势→动作绑定（L2*，同对覆盖式设置走幂等重试）。
    /// gesture: `click|double_click|long_press`；action:
    /// `event_only|record.toggle|stream.toggle|capture.toggle`。立即生效并持久化（重启保留）。
    pub async fn button_bind_set(&self, gesture: &str, action: &str) -> Result<Value> {
        self.request_idem(
            "button.bind.set",
            json!({ "gesture": gesture, "action": action }),
            Duration::from_secs(5),
        )
        .await
    }

    /// `record.delete` 删除录音分段（L2*；正在录音的当前分段设备端拒 3001）。
    /// 返回 `{deleted:true, freed_bytes}`。
    pub async fn record_delete(&self, name: &str) -> Result<Value> {
        self.request(
            "record.delete",
            json!({ "name": name }),
            Duration::from_secs(5),
        )
        .await
    }

    /// `upload.config.get` 查询上传配置（L1）：生效 URL / 是否被覆盖 / 默认地址 / token 是否已设。
    pub async fn upload_config_get(&self) -> Result<Value> {
        self.request_idem("upload.config.get", json!({}), Duration::from_secs(5))
            .await
    }

    /// `upload.config.set` 设置上传 endpoint（L2*；`url=None` 不动，`Some("")` 清覆盖回落默认；
    /// token 同理，作 `authorization: Bearer` 头）。持久化重启保留。
    pub async fn upload_config_set(&self, url: Option<&str>, token: Option<&str>) -> Result<Value> {
        let mut params = serde_json::Map::new();
        if let Some(u) = url {
            params.insert("url".into(), json!(u));
        }
        if let Some(t) = token {
            params.insert("token".into(), json!(t));
        }
        self.request(
            "upload.config.set",
            Value::Object(params),
            Duration::from_secs(5),
        )
        .await
    }

    /// `upload.start` 触发录音上传（L2*；`name=None` 上传全部未上传）。
    /// **快速受理**：传输在设备后台执行，进度经 `upload.state` 事件（subscribe_events 透传），
    /// 汇总状态用 [`DyjDevice::upload_status`]。成功即删源释放存储（失败保留）。
    pub async fn upload_start(&self, name: Option<&str>) -> Result<Value> {
        let params = match name {
            Some(n) => json!({ "name": n }),
            None => json!({}),
        };
        self.request("upload.start", params, Duration::from_secs(5))
            .await
    }

    /// `upload.status` 上传状态（uploading/current_file/pending/uploaded/last）。
    pub async fn upload_status(&self) -> Result<Value> {
        self.request_idem("upload.status", json!({}), Duration::from_secs(5))
            .await
    }

    /// `ota.check` 检查 OTA 清单（L1，10s：外网 HTTPS）。
    /// 返回 `{need_update, current{version_name,version_code}, remote{version_code,url,sha256?,size?,force?}}`。
    pub async fn ota_check(&self) -> Result<Value> {
        self.request_idem("ota.check", json!({}), Duration::from_secs(10))
            .await
    }

    /// `ota.install` 触发 OTA 安装（**破坏性：覆盖安装并杀死进程**，设备自拉起回新版本）。
    /// `confirm` 必须显式 true（设备端缺 confirm 拒 2003）。快速受理，进度经 `ota.progress` 事件；
    /// `installing` 事件后链路会因进程死亡断开（`DeviceEvent::Disconnected` 属预期）。
    pub async fn ota_install(&self, confirm: bool) -> Result<Value> {
        self.request(
            "ota.install",
            json!({ "confirm": confirm }),
            Duration::from_secs(5),
        )
        .await
    }

    /// `ota.status` OTA 状态（state: idle|checking|downloading|verifying|installing|failed）。
    pub async fn ota_status(&self) -> Result<Value> {
        self.request_idem("ota.status", json!({}), Duration::from_secs(5))
            .await
    }

    /// `ota.config.get` 查询 OTA 清单 URL（三级读取：覆盖→资源默认→兜底）。
    pub async fn ota_config_get(&self) -> Result<Value> {
        self.request_idem("ota.config.get", json!({}), Duration::from_secs(5))
            .await
    }

    /// `ota.config.set` 覆盖 OTA 清单 URL（空串清覆盖回落默认；持久化。防变砖约束：改址命令保留）。
    pub async fn ota_config_set(&self, url: &str) -> Result<Value> {
        self.request(
            "ota.config.set",
            json!({ "url": url }),
            Duration::from_secs(5),
        )
        .await
    }

    /// 幂等命令的超时重试（协议 §4：超时未收到 RSP 可重试）。
    /// 真机实测（2026-10-26，SPRD）：RSP+事件突发后紧跟的下一帧响应会被栈吞掉
    /// （设备端 notify 返回 true 但不上空口）——重试一次即恢复。
    /// 仅用于天然幂等的命令（查询 / 同 owner 覆盖式 set / 可重复 clear）。
    async fn request_idem(&self, cmd: &str, params: Value, timeout: Duration) -> Result<Value> {
        match self.request(cmd, params.clone(), timeout).await {
            Err(SdkError::Timeout { command, seconds }) => {
                tracing::warn!(target: "dyj.device", cmd = %command, "response lost (SPRD notify drop?); retrying once");
                self.request(cmd, params, Duration::from_secs(seconds))
                    .await
            }
            other => other,
        }
    }

    /// `device.info` 设备基础信息（L1）。verbose=true 附开发排障字段。
    pub async fn device_info(&self, verbose: bool) -> Result<Value> {
        self.request(
            "device.info",
            json!({ "verbose": verbose }),
            Duration::from_secs(5),
        )
        .await
    }

    /// `device.battery` 电量与充电状态（L1）。
    pub async fn battery(&self) -> Result<Value> {
        self.request("device.battery", json!({}), Duration::from_secs(5))
            .await
    }

    /// `device.storage` 存储余量（L1）。
    pub async fn storage(&self) -> Result<Value> {
        self.request("device.storage", json!({}), Duration::from_secs(5))
            .await
    }

    /// `camera.status` 摄像头状态（L1）。
    /// 返回 `{available, torch, recording, photo_count, video_count}`。
    pub async fn camera_status(&self) -> Result<Value> {
        self.request("camera.status", json!({}), Duration::from_secs(5))
            .await
    }

    /// `camera.capture` 拍照（L2*，10s：camera2 open+capture 余量）。
    /// 返回 `{name, bytes}`；文件落 `files/camera`，经 upload.start 走 WiFi 上传（BLE 不传媒体）。
    pub async fn camera_capture(&self) -> Result<Value> {
        self.request("camera.capture", json!({}), Duration::from_secs(10))
            .await
    }

    /// `camera.record.start` 开始录像（L2*；纯视频不含音频；max_s 到时自动停）。
    /// `max_s`：录像时长上限，缺省 600，上限 1800；返回 `{recording, name}`。
    pub async fn camera_record_start(&self, max_s: Option<u64>) -> Result<Value> {
        let params = max_s
            .map(|s| json!({ "max_s": s }))
            .unwrap_or_else(|| json!({}));
        self.request("camera.record.start", params, Duration::from_secs(5))
            .await
    }

    /// `camera.record.stop` 停止录像（L2*）。返回 `{name, bytes, duration_s}`。
    pub async fn camera_record_stop(&self) -> Result<Value> {
        self.request("camera.record.stop", json!({}), Duration::from_secs(5))
            .await
    }

    /// `camera.record.status` 录像状态（L1）。返回 `{recording, name?, elapsed_s, max_s}`。
    pub async fn camera_record_status(&self) -> Result<Value> {
        self.request("camera.record.status", json!({}), Duration::from_secs(5))
            .await
    }

    /// `camera.list` 相机文件列表（L1；indexed 流式聚合）。
    /// 返回 `{total, items:[{name, bytes, mtime, kind:"photo"|"video"}]}`（时间升序）。
    pub async fn camera_list(&self) -> Result<Value> {
        self.request_idem("camera.list", json!({}), Duration::from_secs(10))
            .await
    }

    /// `camera.delete` 删除相机文件（L2*）。返回 `{deleted, freed_bytes}`；
    /// 白名单外/不存在报 1002，删录像中当前文件报 3001。
    pub async fn camera_delete(&self, name: &str) -> Result<Value> {
        self.request(
            "camera.delete",
            json!({ "name": name }),
            Duration::from_secs(5),
        )
        .await
    }

    /// `camera.torch` 闪光灯常亮开关（L2*）。capture/record 进行中报 3001（闪光单元占用）。
    pub async fn camera_torch(&self, on: bool) -> Result<Value> {
        self.request("camera.torch", json!({ "on": on }), Duration::from_secs(5))
            .await
    }

    /// `wifi.scan` 扫描周围 WiFi（L1，35s：设备端含 SPRD startScan 等待；indexed 流式聚合；
    /// 幂等查询走超时重试）。
    /// 返回 `{total, items:[{ssid,bssid,rssi,security,frequency}]}`（rssi 降序、同名去重）；
    /// 设备定位开关关闭时报错 3401 location_required。
    pub async fn wifi_scan(&self) -> Result<Value> {
        self.request_idem("wifi.scan", json!({}), Duration::from_secs(35))
            .await
    }

    /// `wifi.connect` 连接 WiFi（L2*，35s：设备端同步等 COMPLETED+DHCP）。
    /// 返回 `{connected, ssid, ip?, phase?}`；失败 connected=false 带阶段
    /// phase（auth=密码错/dhcp/timeout），设备端自动清理并恢复原网络。
    pub async fn wifi_connect(
        &self,
        ssid: &str,
        password: Option<&str>,
        bssid: Option<&str>,
    ) -> Result<Value> {
        let mut params = serde_json::Map::new();
        params.insert("ssid".into(), json!(ssid));
        if let Some(p) = password {
            params.insert("password".into(), json!(p));
        }
        if let Some(b) = bssid {
            params.insert("bssid".into(), json!(b));
        }
        self.request(
            "wifi.connect",
            Value::Object(params),
            Duration::from_secs(35),
        )
        .await
    }

    /// `wifi.status` WiFi 状态（L1，幂等查询走超时重试）。
    /// 返回 `{wifi_enabled, state, ssid?, bssid?, ip?, rssi?, link_speed_mbps?, frequency?, gateway?}`。
    pub async fn wifi_status(&self) -> Result<Value> {
        self.request_idem("wifi.status", json!({}), Duration::from_secs(5))
            .await
    }

    /// `wifi.forget` 删除已保存网络（L2*）。`ssid=None` 删除全部。
    pub async fn wifi_forget(&self, ssid: Option<&str>) -> Result<Value> {
        let mut params = serde_json::Map::new();
        if let Some(s) = ssid {
            params.insert("ssid".into(), json!(s));
        }
        self.request("wifi.forget", Value::Object(params), Duration::from_secs(5))
            .await
    }

    /// `ap.start` 开热点（L2*，35s：设备端含 ENABLED 等待 + STA 存活探测；幂等重试兜吞帧）。
    /// 返回 `{active, ssid, password, ip, sta_dropped}`；SPRD 不支持并发时若未
    /// `allow_sta_drop` 会自动回滚并报错 3502 sta_concurrency_unsupported。
    /// （设备端 start 幂等：先关旧热点再开，超时重试安全；RSP 紧跟 conn.wifi 事件突发，
    /// 是 SPRD FF11 吞帧高发位——2026-10-26 真机实测。）
    pub async fn ap_start(
        &self,
        ssid: Option<&str>,
        password: Option<&str>,
        allow_sta_drop: bool,
    ) -> Result<Value> {
        let mut params = serde_json::Map::new();
        if let Some(s) = ssid {
            params.insert("ssid".into(), json!(s));
        }
        if let Some(p) = password {
            params.insert("password".into(), json!(p));
        }
        if allow_sta_drop {
            params.insert("allow_sta_drop".into(), json!(true));
        }
        self.request_idem("ap.start", Value::Object(params), Duration::from_secs(35))
            .await
    }

    /// `ap.stop` 关热点（L2*，幂等重试兜吞帧）。STA 由系统自动恢复；8788 文件服务随停。
    pub async fn ap_stop(&self) -> Result<Value> {
        self.request_idem("ap.stop", json!({}), Duration::from_secs(5))
            .await
    }

    /// `ap.status` 热点状态（L1，幂等查询走超时重试）。
    /// 返回 `{active, ssid?, ip?, sta_state}`；ip 可直接喂 [`crate::access_point::ApClient`]。
    pub async fn ap_status(&self) -> Result<Value> {
        self.request_idem("ap.status", json!({}), Duration::from_secs(5))
            .await
    }

    /// `proto.echo` 回声联调（L0）。
    pub async fn echo(&self, message: &str) -> Result<Value> {
        self.request(
            "proto.echo",
            json!({ "message": message }),
            Duration::from_secs(10),
        )
        .await
    }
}

pub struct DeviceManager {
    adapter: btleplug::platform::Adapter,
}

impl DeviceManager {
    pub async fn new() -> Result<DeviceManager> {
        let manager = btleplug::platform::Manager::new()
            .await
            .map_err(|e| SdkError::BluetoothUnavailable(e.to_string()))?;
        let adapter = manager
            .adapters()
            .await
            .map_err(|e| SdkError::BluetoothUnavailable(e.to_string()))?
            .into_iter()
            .next()
            .ok_or_else(|| SdkError::BluetoothUnavailable("no adapter".into()))?;
        Ok(DeviceManager { adapter })
    }

    pub async fn scan(
        &self,
        name_prefix: &str,
        duration: Duration,
    ) -> Result<Vec<transport::DiscoveredDevice>> {
        transport::scan(&self.adapter, name_prefix, duration).await
    }

    /// 连接设备：连接 + 服务发现 + 订阅 + reader 任务（连接即完成全部握手）。
    pub async fn connect_by_name(&self, name: &str) -> Result<DyjDevice> {
        let found = transport::scan(&self.adapter, name, Duration::from_secs(5)).await?;
        let target = found
            .into_iter()
            .find(|d| d.name == name)
            .ok_or_else(|| SdkError::NotFound(name.to_string()))?;
        self.connect(&target.id, &target.name).await
    }

    pub async fn connect(
        &self,
        id: &btleplug::platform::PeripheralId,
        name: &str,
    ) -> Result<DyjDevice> {
        let (frame_tx, mut frame_rx) = mpsc::channel::<String>(64);
        let (binary_tx, mut binary_rx) = mpsc::channel::<crate::protocol::BinaryFrame>(256);
        let transport =
            StdArc::new(transport::connect(&self.adapter, id, frame_tx, binary_tx).await?);

        let (event_tx, _) = broadcast::channel(64);
        let (audio_tx, _) = broadcast::channel::<crate::audio::PcmChunk>(256);
        let pending: Arc<PendingMap> = Arc::new(PendingMap(Mutex::new(HashMap::new())));
        let pending_reader = pending.clone();
        let event_tx_reader = event_tx.clone();
        let device_name = name.to_string();
        // keepalive：sender 被 drop 时 reader 退出（设备对象析构 = 通道关闭）
        let (keep_tx, mut keep_rx) = mpsc::channel::<()>(1);
        let keepalive = Arc::new(keep_tx);

        // 连接状态监视（双保险）：部分断连形态下 notifications 流不会立即结束（对端离开
        // 范围/僵尸链路），reader 的 Disconnected 永远不来。周期查询底层链路状态，
        // 翻转 false 即广播 Disconnected（事件幂等，订阅方重复处理无害）。显式 disconnect()
        // 后链路必然 false → 监视任务自然收尾。
        {
            let monitor_transport = transport.clone();
            let event_tx_monitor = event_tx.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    match monitor_transport.is_connected().await {
                        Ok(true) => continue,
                        Ok(false) => {
                            tracing::info!(target: "dyj.device", "link monitor: disconnected");
                            let _ = event_tx_monitor.send(DeviceEvent::Disconnected);
                            break;
                        }
                        Err(_) => continue, // 状态查询瞬时失败（断开进行中）：下一轮再确认
                    }
                }
            });
        }

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    line = frame_rx.recv() => {
                        match line {
                            Some(line) => match Frame::decode(&line) {
                                Some(Frame::Response { id, code, msg, data }) => {
                                    let frame = Frame::Response { id, code, msg, data: data.clone() };
                                    // 流式响应（§3.3 more:true）保留 pending 条目继续收帧
                                    let sender = {
                                        let mut pending = pending_reader.0.lock().unwrap();
                                        if response_done(&data) { pending.remove(&id) } else { pending.get(&id).cloned() }
                                    };
                                    if let Some(tx) = sender {
                                        let _ = tx.send(frame).await;
                                    }
                                }
                                Some(Frame::Event { evt, ts, data }) => {
                                    let _ = event_tx_reader.send(DeviceEvent::Event { evt, ts, data });
                                }
                                _ => { /* 非法帧丢弃并计数（规则 §3.1） */ }
                            },
                            None => {
                                let _ = event_tx_reader.send(DeviceEvent::Disconnected);
                                break;
                            }
                        }
                    }
                    _ = keep_rx.recv() => break,
                }
            }
        });

        // 音频解码任务：FF12 音频帧统一解码为 16kHz PCM。
        // type=0x04 AMR-WB：payload 按帧长表切帧逐帧解码（每帧 320 样本 / 20ms）；
        // type=0x01 ADPCM：整体流式解码（状态连续）。seq 断流即重置解码器。
        let audio_tx_task = audio_tx.clone();
        tokio::spawn(async move {
            let mut amr = crate::audio::AmrWbDecoder::new();
            let mut adpcm = crate::audio::AdpcmDecoder::new();
            let mut last_seq: Option<u16> = None;
            while let Some(frame) = binary_rx.recv().await {
                let audio_type = match frame.frame_type {
                    crate::protocol::BinaryFrame::TYPE_REALTIME_AUDIO_AMR => "amr-wb",
                    crate::protocol::BinaryFrame::TYPE_REALTIME_AUDIO => "adpcm",
                    _ => continue, // 文件块/心跳等不进音频通道
                };
                if last_seq.is_none() {
                    tracing::info!(target: "dyj.device", first_seq = frame.seq, encoding = audio_type, "first audio frame received");
                }
                let discontinuous = match last_seq {
                    Some(last) => frame.seq != last.wrapping_add(1),
                    None => false,
                };
                last_seq = Some(frame.seq);

                let mut samples: Vec<i16> = Vec::new();
                if frame.frame_type == crate::protocol::BinaryFrame::TYPE_REALTIME_AUDIO_AMR {
                    if discontinuous {
                        tracing::warn!(target: "dyj.device", seq = frame.seq, "audio stream gap; resetting decoder");
                        amr.reset();
                    }
                    let mut rest = frame.payload.as_slice();
                    while !rest.is_empty() {
                        let mode = (rest[0] >> 3) as usize & 0x0F;
                        let bad = mode > 8 || rest.len() < 1 + crate::audio::WB_PAYLOAD_BYTES[mode];
                        if bad {
                            tracing::warn!(target: "dyj.device", toc = format_args!("0x{:02x}", rest[0]), "bad AMR frame; drop rest of block");
                            break;
                        }
                        let (one, remainder) =
                            rest.split_at(1 + crate::audio::WB_PAYLOAD_BYTES[mode]);
                        if let Some(pcm) = amr.decode(one) {
                            samples.extend(pcm);
                        }
                        rest = remainder;
                    }
                } else {
                    if discontinuous {
                        tracing::warn!(target: "dyj.device", seq = frame.seq, "audio stream gap; resetting decoder");
                        adpcm.reset();
                    }
                    adpcm.decode_chunk(&frame.payload, &mut samples);
                }

                if samples.is_empty() && !discontinuous {
                    continue;
                }
                let chunk = crate::audio::PcmChunk {
                    samples,
                    sample_rate: crate::audio::SAMPLE_RATE_HZ,
                    channels: crate::audio::CHANNELS,
                    discontinuous,
                };
                let _ = audio_tx_task.send(chunk); // 无订阅者/满员时静默丢弃
            }
        });

        Ok(DyjDevice {
            name: device_name,
            transport,
            pending,
            next_id: Arc::new(AtomicU64::new(0)),
            event_tx,
            audio_tx: Arc::new(audio_tx),
            _keepalive: keepalive,
        })
    }
}

/// 实时音频会话（RAII）：start 开流 → [`Self::next_chunk`] 持续取 PCM → drop 自动 stop。
/// 断连后 `next_chunk` 返回 Err（Lagged = 消费太慢丢块，可继续收后续块）。
pub struct RealtimeAudioSession {
    device: DyjDevice,
    rx: broadcast::Receiver<crate::audio::PcmChunk>,
    rx_chunk_count: u64,
}

impl RealtimeAudioSession {
    /// 开启音频流并订阅。设备端 ADPCM 在 SDK 内解码为 16kHz PCM。
    pub async fn start(device: &DyjDevice) -> Result<Self> {
        device.audio_stream_start().await?;
        Ok(Self {
            device: device.clone(),
            rx: device.subscribe_audio(),
            rx_chunk_count: 0,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<crate::audio::PcmChunk> {
        self.rx.resubscribe()
    }

    /// 收下一个 PCM 块（AMR-WB 路径 ≈100ms/960 样本；断流块 samples 为空仅标记）。
    pub async fn next_chunk(&mut self) -> Result<crate::audio::PcmChunk> {
        let chunk = match self.rx.recv().await {
            Ok(chunk) => chunk,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                return Err(SdkError::AudioLagged { lost_chunks: n });
            }
            Err(broadcast::error::RecvError::Closed) => {
                return Err(SdkError::Disconnected {
                    command: "audio.stream".into(),
                });
            }
        };
        self.rx_chunk_count += 1;
        if self.rx_chunk_count.is_multiple_of(250) {
            // 每 250 块（5s）打一条心跳日志，确认流健康
            tracing::debug!(target: "dyj.device", chunks = self.rx_chunk_count, "audio flowing");
        }
        Ok(chunk)
    }
}

impl Drop for RealtimeAudioSession {
    fn drop(&mut self) {
        let device = self.device.clone();
        tokio::spawn(async move {
            let _ = device.audio_stream_stop().await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_done_semantics_match_protocol_3_3() {
        // 普通单帧响应（无 more 字段）→ 终结
        assert!(response_done(&json!({ "index": 0 })));
        // 流式帧 more:true → 继续
        assert!(!response_done(
            &json!({ "index": 0, "more": true, "entry": {} })
        ));
        // 收尾帧 more:false → 终结
        assert!(response_done(
            &json!({ "index": 3, "more": false, "total": 3 })
        ));
        // 非布尔 more 视为终结（严格类型，前向兼容坏帧不挂死）
        assert!(response_done(&json!({ "index": 0, "more": "true" })));
        assert!(response_done(&json!({})));
    }
}
