# reai-dyj-sdk

点一机 1 代（DianYiJi V1）对外接入 SDK：Rust async client，跑在宿主侧（macOS / Linux / Windows 桌面），经 **BLE（GATT Central）** 连接点一机 1 代设备，把硬件能力——设备信息、WiFi 配网、录音与实时音频、LED、物理按键、录音上传、OTA 升级、AP 热点文件快传——以类型化 API 提供给外部开发者。自带全 API 覆盖的命令行工具 `dyj_cli`。

> **协议契约**：本 SDK 是协议 `proto_version=1` 的参考实现；帧格式、命令表、错误码、超时表、AP 附属 HTTP 端点的单一事实源是 [PROTOCOL.md](PROTOCOL.md)。
> **版本配套**：SDK 0.1.6 ↔ 设备固件 app 0.8.7。各版本配套固件与真机验证状态见 [CHANGELOG.md](CHANGELOG.md)。
> 设备固件（Android 系统应用，不开源）所在仓库经 **git submodule** 引用本仓库（`sdk/` 路径）——本仓库是 SDK 的唯一源码仓库。

## 安装

```toml
[dependencies]
reai-dyj-sdk = "0.1.6"
```

或跟踪仓库最新（git 依赖）：

```toml
reai-dyj-sdk = { git = "https://github.com/ReAI-com/dyj-v1-sdk" }
```

要求 Rust ≥ 1.93。BLE 走 btleplug（macOS/Windows/Linux/Android 宿主均可；**Linux 编译需 libdbus-1-dev**）；AP 文件快传走 reqwest（rustls，无 OpenSSL 依赖）；实时音频解码内置 vendor 的 opencore-amrwb（Apache-2.0，FFI 编译需 C++ 编译器）。

命令行工具：

```bash
cargo install reai-dyj-sdk    # 安装 dyj_cli 到 ~/.cargo/bin
# 或源码构建：
git clone https://github.com/ReAI-com/dyj-v1-sdk && cd dyj-v1-sdk && cargo build --release
```

## 快速上手

```rust
use reai_dyj_sdk::DeviceManager;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 发现并连接（设备广播名 DYJ_ 前缀 + 设备标识后缀）
    let manager = DeviceManager::new().await?;
    let device = manager.connect_by_name("DYJ_RA6BO5").await?;
    // connect() 内已完成：服务发现 → 订阅 notify → MTU 协商 → hello 握手（能力协商 feature_bits）

    // 2. 命令均为异步方法，返回 serde_json::Value
    let info = device.device_info(false).await?;
    println!("设备 {} 固件 {}", info["data"]["device_id"], info["data"]["app_version"]);

    // 3. 事件订阅（按键/录音分段/WiFi 状态/上传进度/OTA 进度…）
    let mut events = device.subscribe_events();
    tokio::spawn(async move { while let Ok(evt) = events.recv().await { /* … */ } });

    // 4. 实时音频（AMR-WB 设备端编码，SDK 解码为 16kHz 单声道 PCM）
    device.audio_stream_start().await?;
    let mut pcm = device.subscribe_audio();
    while let Ok(chunk) = pcm.recv().await { /* chunk: 16kHz i16 PcmChunk */ }

    // 5. AP 热点快传（BLE 仅 ~3.6KB/s；AP 下 HTTP 全速）
    let ap = device.ap_start(None, None, false).await?;
    let client = reai_dyj_sdk::ApClient::new(ap["ip"].as_str().unwrap(), 8788)?;
    for f in client.list_files().await?.files {
        client.download_to_dir(&f.name, "./recordings").await?;
    }
    device.ap_stop().await?;
    device.disconnect().await?;
    Ok(())
}
```

完整可跑示例见 `examples/`（均单连接、带超时，可直接作为验收脚本）：
`wifi_sta.rs`（配网全流程）、`wifi_ap.rs`（AP 快传）、`led_check.rs`、`button_test.rs`、`dual_tap.rs`、`upload_test.rs`（上传全链路）、`ota_test.rs`（OTA 全链路）。

## API 面

入口：`DeviceManager`（扫描/连接）→ `DyjDevice`（能力 API）。方法名即协议命令名（`device.info` → `device_info()`），响应为协议 `data` 原样 JSON；列表/大响应的 indexed 分片（PROTOCOL.md §3.3）已在 SDK 内聚合成完整结构。

| 能力域 | 方法 | 说明 |
|---|---|---|
| 连接 | `DeviceManager::new / scan / connect_by_name / connect` | hello 握手 + feature_bits 能力协商一次完成 |
| 设备 | `device_info(verbose)` `battery()` `storage()` | 身份/电量/存储 |
| 实时音频 | `audio_stream_start/stop()` `subscribe_audio()` `AudioStream::next_chunk()` | AMR-WB → 16kHz PCM 解码内置 |
| 采集引擎 | `audio_capture_start/stop()` | stream/record 共用数据源开关 |
| 录音 | `record_start(segment_min, session)` `record_stop()` `record_list()` `record_status()` `record_delete(name)` | 10min 分段、会话命名、删除 |
| 上传 | `upload_config_get/set(url, token)` `upload_start(name)` `upload_status()` | endpoint 可配置；成功删源语义 |
| OTA | `ota_check()` `ota_install(confirm)` `ota_status()` `ota_config_get/set(url)` | `confirm:false` 会被设备拒绝（2003） |
| LED | `led_status()` `led_set(pattern, brightness, priority, timeout_s)` `led_clear()` | claim 仲裁，ble-override 归属 |
| 按键 | `button_status()` `button_bind_get()` `button_bind_set(gesture, action)` | 手势→动作绑定表持久化 |
| WiFi | `wifi_scan()` `wifi_connect(ssid, password, bssid)` `wifi_status()` `wifi_forget(ssid)` | 扫描结果 rssi 降序去重 |
| AP 快传 | `ap_start/stop/status()` + `ApClient::list_files / download_to_dir / upload_ota` | HTTP 文件服务 + OTA 包上传（端点见 PROTOCOL.md §11） |
| 事件 | `subscribe_events()` | `DeviceEvent` 单一枚举，未知事件 `Unhandled` 兜底 |
| 通用 | `request(cmd, params, timeout)` | 未封装命令的透传出口 |

### 事件

`subscribe_events()` 返回 `broadcast::Receiver<DeviceEvent>`：`Button`（down/up/click/double_click/long_press_*）、`RecordSegment`、`LedState`、`ConnWifi`、`UploadState`、`OtaProgress` 等；高频 PCM 永不进事件总线（独立 `subscribe_audio()` 通道）。

### 命令行工具

```bash
dyj_cli scan                          # 扫描周围 DYJ_ 设备
dyj_cli hello                         # 握手看能力位
dyj_cli wifi scan|connect|status|forget
dyj_cli ap start|stop|status|files|download|ota   # ota 子命令：AP 通道上传安装包
dyj_cli led status|set|clear
dyj_cli button status|bind|set
dyj_cli record list|delete
dyj_cli upload status|config|set|start
dyj_cli ota check|install|status|config|set
dyj_cli audio [秒] [输出.wav]          # 实时音频录 wav
dyj_cli send <cmd> <params-json>      # 任意命令透传
```

## 已知约束（协议层决定，双侧一致）

- **命令严格串行**：设备端单协议 worker，长命令（wifi.scan/connect、ap.start 35s）期间其他命令排队；客户端超时表见 PROTOCOL.md §7（默认 10s）
- **单 BLE 连接**：设备同一时间只接受 1 个 GATT 主连接
- **BLE 链路 ~3.6KB/s**（SPRD 平台实测）：大文件走 AP 快传，BLE 只做控制与小数据
- **写串行队列 + MTU-3 分片**已在 SDK 内处理；iOS 宿主注意订阅后 ≥1500ms 再发首条写（SDK 已内置）
- **破坏性命令双侧确认**：`ota.install(confirm)` 的 confirm 位与设备端校验对齐

## License

[MIT](LICENSE)。第三方组件：`vendor/opencore-amr`（AMR-WB 解码器，Apache-2.0，见 `vendor/opencore-amr/LICENSE`）。
