# Changelog

版本与设备固件（Android 系统应用，不开源）按批配套发布：SDK 0.1.x ↔ app 0.8.x 同批落地、同批真机验证。协议契约见 [PROTOCOL.md](PROTOCOL.md)。

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
