# Changelog

版本与设备固件（Android 系统应用，不开源）按批配套发布：SDK 0.1.x ↔ app 0.8.x 同批落地、同批真机验证。协议契约见 [PROTOCOL.md](PROTOCOL.md)。

## 0.1.11 — 2026-10-30（配套 app 0.8.11）

- **系统控制域**（批 3）：`vibrator_once` / `vibrator_cancel`（VibrationEffect 一次性震动，
  时长 1..10000ms / 强度 1..255）、`volume_get` / `volume_set`（music/ring/system 三流，
  set 后回读生效值，越界 clamp）、`device_time`（回读 + AlarmManager 对时——联网自动对时
  会被系统覆盖）、`device_reboot(confirm)`（**全协议唯一 L3**：缺 confirm 报 2003，accepted
  后延迟 1s 重启，BLE 断连属预期；REBOOT 签名权限仅平台签名 sys 包持有）
- CLI：`vibrator once|cancel`、`volume get|set`、`device time|reboot`
- 示例：`sysctrl_test.rs`（安全路径：震动体感 / 音量回读 / 对时偏差 <1s / confirm 缺失
  2003；真实重启单独走 `dyj_cli device reboot`）

## 0.1.10 — 2026-10-30（配套 app 0.8.10，SDK 侧修复）

- **透传超时表 `passthrough_timeout(cmd)`**（新公开 API）：`send` 透传/自建通道的客户端
  超时必须 ≥ 设备端登记超时（§7）——`location.get` 设备端等 GPS 最长 30s，此前 CLI
  `send` 与 console `/api/command` 统一 10s 提前掐断，正常业务响应 `has_fix:false`
  被丢（10s 处超时而非 15-30s 处返回）。typed 方法不受影响；长命令分档
  location.get 32s / wifi.scan·connect·ap.start 37s / 10s 级 12s / 默认 10s

## 0.1.9 — 2026-10-30（配套 app 0.8.10）

- **sensor 域**（真机 HAL 实注册 5 类）：`sensor_status` / `sensor_read`（单次实读，
  未即时交付的类型字段省略不报错；`steps` 为开机累计值）/ `sensor_stream_start` /
  `sensor_stream_stop`（EVT `sensor.data` 一帧合并多类型，500ms 下限节流防 BLE 洪泛）。
  加速度计为桩（"NULL Sensor"）、光感/距离感无 HAL → 不在 available，请求报 1002
- **location 域**：`location_status` / `location_get`（无 fix 正常返回 `{has_fix:false}`
  不报错——室内无星常态；设备端前置自动开定位模式）/ `location_stream_start` /
  `location_stream_stop`（EVT `location.fix` 仅新 fix）。`wifi.scan` 的 3401 行为不受影响
- CLI：`sensor status|read|stream start/stop`、`location status|get|stream start/stop`
- 示例：`sensor_test.rs`（量级判读 + 节流帧率 + 桩拒绝）、`location_test.rs`
  （TTFF 基线 + 无 fix 语义），均单连接
- 清理残留：`camera_torch` 方法与 CLI `camera torch` 子命令移除（0.1.7 漏网——
  真机确认无闪光灯硬件，该命令未进任何真机验证路径）

## 0.1.8 — 2026-10-29（配套 app 0.8.9）

- **录像修复配套**（SDK 接口无变化，0.1.7 全部能力继承）：
  - 录像输出改设备公共存储 `DYJ/` 目录（SELinux mediaserver 拒应用私有目录 fd——
    ROM 策略不可改，`setOutputFile` 路径/fd 双路被拒 prepare -38）
  - 码率钳 4Mbps（HIGH 档原生 16Mbps 超服务器 16MB 单文件上限）；规格回退链逐档 prepare
  - 真机全链路验证：record start/stop（17s/7MB）→ list → delete

## 0.1.7 — 2026-10-29（配套 app 0.8.8）

- **camera 域**：`camera_status` / `camera_capture`（拍照落盘 JPEG，EXIF 摆正）/
  `camera_record_start` / `camera_record_stop` / `camera_record_status`（MediaRecorder
  纯视频，max_s 到时自动停，码率钳 4Mbps，输出设备公共存储 `DYJ/` 目录——
  SELinux mediaserver 不收应用私有目录的 fd，规格回退链逐档 prepare）/ `camera_list`（indexed 流式聚合）/ `camera_delete`。
  capture/record 域内互斥，占用中报 3001；拍照/上传/删除/录像已真机验证
- 设备端上传白名单放宽：`upload.start` 未传集合自动涵盖相机媒体
  （`img_*.jpg` / `video_*.mp4`），照片/视频经 WiFi 上传（BLE 链路不传媒体）
- CLI：`camera status|capture|list|delete|record start/stop/status`
- 示例：`camera_test.rs`（拍照→列表→短录像→清理，单连接）
- **server 模块**：`ServerClient`（远端上传服务器客户端：`list_files` /
  `download_file_to` / `download_to_dir`，纯 HTTP 与 BLE 无关）+ CLI
  `server files|pull`；配套 `examples/media_server.rs` 单文件自部署媒体服务器
  （上传/列表/下载同契约，sha256 回执 + 16MB 上限 + 可选 Bearer 鉴权）
- 相机已真机验证（拍照/上传/删除/录像）；**camera.torch 闪光灯已移除**——真机确认
  无闪光灯硬件（HAL FLASH_INFO_AVAILABLE=true 为虚报），该命令未进任何发布版本

## 0.1.6 — 2026-10-27（配套 app 0.8.7）

- **upload 域**：`record_delete` / `upload_config_get` / `upload_config_set`（endpoint
  可配置，token 作 Bearer 头）/ `upload_start`（缺省=全部未上传；成功删源）/
  `upload_status`；`upload.state` 事件透传
- **ota 域**：`ota_check` / `ota_install(confirm)`（五重校验：size/sha256/包名/签名/
  versionCode 单调）/ `ota_status` / `ota_config_get` / `ota_config_set`；`ota.progress`
  事件透传
- 公开仓治理：LICENSE（MIT）、PROTOCOL.md、CI（fmt/clippy/test）

## 0.1.5 — 2026-10-27（配套 app 0.8.5）

- upload/ota 批次 SDK 前置：事件订阅模型（`subscribe_events`）与命令台轮询避让

## 0.1.4 — 2026-10-26（配套 app 0.8.4）

- **button 域**：`button_status` / `button_bind_get` / `button_bind_set`；
  `button.event`（down/up/click/double_click/long_press 全手势 + 时长）事件透传

## 0.1.3 — 2026-10-26（配套 app 0.8.3）

- **ap 域**：`ap_start` / `ap_stop` / `ap_status`（SPRD 平台不支持 AP+STA 并发，
  未放行自动回滚报 3502）；`ApClient`（AP 内 HTTP 文件服务：列表/下载）

## 0.1.2 — 2026-10-26（配套 app 0.8.2）

- **wifi 域**：`wifi_scan`（indexed 聚合，定位开关关闭报 3401）/ `wifi_connect` /
  `wifi_status` / `wifi_forget`；`conn.wifi` 事件透传

## 0.1.1 — 2026-10-26（配套 app 0.8.1）

- **led 域**：`led_status`（indexed 聚合并重塑 `{active, claims}`）/ `led_set` /
  `led_clear`；`led.state` 事件透传

## 0.1.0 — 2026-10-24（配套 app 0.8.0）

- 首版：BLE 传输（btleplug）+ NUS 文本帧协议（CMD/RSP/EVT）+ `proto.hello` 能力协商
- device 域：`device_info` / `battery` / `storage`
- audio 域：实时音频（AMR-WB 16kHz）`audio_stream_start/stop`；AMR-WB→PCM 解码
  （vendor opencore-amr，Apache-2.0）+ `subscribe_audio()`
- CLI `dyj_cli`（scan/hello/echo/send/audio 及各域子命令）
