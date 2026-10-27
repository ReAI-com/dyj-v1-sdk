//! dyj_cli — 点一机 1 代调试 CLI（覆盖全部已实施协议命令，自动验收入口）。
//!
//! 用法：
//!   dyj_cli scan [前缀]                     扫描设备
//!   dyj_cli hello [设备名]                  连接 + 握手（自动开音频流，断连即停）
//!   dyj_cli echo <消息> [设备名]            连接 + 回声
//!   dyj_cli send <cmd> <params-json> [设备名]  连接 + 发送任意命令
//!   dyj_cli audio [秒] [输出.wav] [设备名]  连接 + 录制实时音频到 WAV
//!   dyj_cli led status [设备名]              当前灯效与 claim 栈
//!   dyj_cli led set <pattern> [brightness] [priority] [timeout_s] [设备名]
//!   dyj_cli led clear [设备名]
//!   dyj_cli button status [设备名]           按键能力（手势/阈值/绑定表/最近事件）
//!   dyj_cli button bind [设备名]             查询手势→动作绑定表
//!   dyj_cli button set <click|double_click|long_press> <event_only|record.toggle|stream.toggle|capture.toggle> [设备名]
//!   dyj_cli record list [设备名]             录音分段列表（indexed 聚合）
//!   dyj_cli record delete <name> [设备名]    删除录音分段
//!   dyj_cli upload status [设备名]           上传状态
//!   dyj_cli upload config [设备名]           上传 endpoint 配置（三级读取）
//!   dyj_cli upload set <url> [token] [设备名]  覆盖上传 endpoint（空串清覆盖）
//!   dyj_cli upload start [name] [设备名]     触发上传（缺省=全部未上传；进度看事件）
//!   dyj_cli ota check [设备名]               检查 OTA 清单
//!   dyj_cli ota install [设备名]             触发安装（自动带 confirm:true；进度看事件）
//!   dyj_cli ota status [设备名]              OTA 状态
//!   dyj_cli ota config [设备名]              OTA 清单 URL 配置
//!   dyj_cli ota set <url> [设备名]           覆盖 OTA 清单 URL（空串清覆盖）
//!   dyj_cli wifi scan [设备名]               扫描周围 WiFi（设备定位开关需开）
//!   dyj_cli wifi connect <ssid> [password] [设备名]   连接 WiFi（缺密码=开放网络）
//!   dyj_cli wifi status [设备名]             WiFi 状态（ssid/ip/rssi…）
//!   dyj_cli wifi forget [ssid|-] [设备名]    删除已保存网络（"-"=全部）
//!   dyj_cli ap start [ssid] [password] [--allow-sta-drop] [设备名]   开热点
//!   dyj_cli ap stop [设备名]                 关热点（STA 自动恢复）
//!   dyj_cli ap status [设备名]               热点状态（ssid/ip/sta_state）
//!   dyj_cli ap files [设备名]                列出热点内文件（HTTP 8788）
//!   dyj_cli ap download <name> <目录> [设备名]  经热点下载录音分段（md5 对账）
//!   dyj_cli ap ota <apk路径> [设备名]        经热点上传 APK 暂存（安装再走 ota install）
//!
//! 设备名默认 DYJ_RA6BO5。

use reai_dyj_sdk::{DeviceManager, SdkError};
use std::time::Duration;

const DEFAULT_DEVICE: &str = "DYJ_RA6BO5";

/// 统一日志格式：`<时间> <级别> <target>: <消息>`，级别由 RUST_LOG 控制（默认 info）。
fn init_logging() {
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(true)
        .init();
}

#[tokio::main]
async fn main() {
    init_logging();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!(
            "用法: dyj_cli <scan|hello|echo|send|audio|led|button|record|upload|ota|wifi|ap> [参数...]\n  scan [前缀]\n  hello [设备名={DEFAULT_DEVICE}]\n  echo <消息> [设备名]\n  send <cmd> <params-json> [设备名]\n  audio [秒] [输出.wav] [设备名]\n  led status [设备名]\n  led set <pattern> [brightness] [priority] [timeout_s] [设备名]\n  led clear [设备名]\n  button status [设备名]\n  button bind [设备名]\n  button set <click|double_click|long_press> <event_only|record.toggle|stream.toggle|capture.toggle> [设备名]\n  record list [设备名]\n  record delete <name> [设备名]\n  upload status [设备名]\n  upload config [设备名]\n  upload set <url> [token] [设备名]\n  upload start [name] [设备名]\n  ota check [设备名]\n  ota install [设备名]\n  ota status [设备名]\n  ota config [设备名]\n  ota set <url> [设备名]\n  wifi scan [设备名]\n  wifi connect <ssid> [password] [设备名]\n  wifi status [设备名]\n  wifi forget [ssid|-] [设备名]\n  ap start [ssid] [password] [--allow-sta-drop] [设备名]\n  ap stop [设备名]\n  ap status [设备名]\n  ap files [设备名]\n  ap download <name> <目录> [设备名]"
        );
        std::process::exit(2);
    }

    let result = match args[0].as_str() {
        "scan" => {
            let prefix = args.get(1).map(|s| s.as_str()).unwrap_or("");
            scan(prefix)
                .await
                .map(|devices| serde_json::json!({ "devices": devices }))
        }
        "hello" => {
            let name = args.get(1).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
            with_device(
                name,
                |device| async move { device.hello("dyj_cli", true).await },
            )
            .await
        }
        "echo" => {
            let message = args.get(1).cloned().unwrap_or_else(|| "ping".into());
            let name = args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
            with_device(
                name,
                move |device| async move { device.echo(&message).await },
            )
            .await
        }
        "send" => {
            let cmd = args.get(1).cloned().unwrap_or_default();
            let params: serde_json::Value = match args
                .get(2)
                .map(|s| serde_json::from_str::<serde_json::Value>(s))
            {
                Some(Ok(v)) => v,
                Some(Err(e)) => {
                    eprintln!("ERROR: params 不是合法 JSON: {e}");
                    std::process::exit(2);
                }
                None => serde_json::json!({}),
            };
            let name = args.get(3).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
            with_device(name, move |device| async move {
                device.request(&cmd, params, Duration::from_secs(10)).await
            })
            .await
        }
        "audio" => {
            let seconds: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(5);
            let out_path = args
                .get(2)
                .cloned()
                .unwrap_or_else(|| "dyj_audio.wav".into());
            let name = args.get(3).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
            audio_capture(name, seconds, &out_path).await
        }
        "led" => {
            let sub = args.get(1).map(|s| s.as_str()).unwrap_or("status");
            match sub {
                "status" => {
                    let name = args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
                    with_device(name, |device| async move { device.led_status().await }).await
                }
                "set" => {
                    let pattern = args.get(2).cloned().unwrap_or_default();
                    if pattern.is_empty() {
                        eprintln!("ERROR: 用法 dyj_cli led set <pattern> [brightness] [priority] [timeout_s] [设备名]");
                        std::process::exit(2);
                    }
                    let brightness = args.get(3).map(|s| s.as_str());
                    let priority = args.get(4).and_then(|s| s.parse().ok());
                    let timeout_s = args.get(5).and_then(|s| s.parse().ok());
                    let name = args.get(6).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
                    with_device(name, move |device| async move {
                        device
                            .led_set(&pattern, brightness, priority, timeout_s)
                            .await
                    })
                    .await
                }
                "clear" => {
                    let name = args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
                    with_device(name, |device| async move { device.led_clear().await }).await
                }
                other => {
                    eprintln!("未知 led 子命令: {other}（status/set/clear）");
                    std::process::exit(2);
                }
            }
        }
        "button" => {
            let sub = args.get(1).map(|s| s.as_str()).unwrap_or("status");
            match sub {
                "status" => {
                    let name = args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
                    with_device(name, |device| async move { device.button_status().await }).await
                }
                "bind" => {
                    let name = args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
                    with_device(name, |device| async move { device.button_bind_get().await }).await
                }
                "set" => {
                    let gesture = args.get(2).cloned().unwrap_or_default();
                    let action = args.get(3).cloned().unwrap_or_default();
                    if gesture.is_empty() || action.is_empty() {
                        eprintln!(
                            "ERROR: 用法 dyj_cli button set <click|double_click|long_press> <event_only|record.toggle|stream.toggle|capture.toggle> [设备名]"
                        );
                        std::process::exit(2);
                    }
                    let name = args.get(4).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
                    with_device(name, move |device| async move {
                        device.button_bind_set(&gesture, &action).await
                    })
                    .await
                }
                other => {
                    eprintln!("未知 button 子命令: {other}（status/bind/set）");
                    std::process::exit(2);
                }
            }
        }
        "record" => {
            let sub = args.get(1).map(|s| s.as_str()).unwrap_or("list");
            match sub {
                "list" => {
                    let name = args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
                    with_device(name, |device| async move { device.record_list().await }).await
                }
                "delete" => {
                    let del_name = args.get(2).cloned().unwrap_or_default();
                    if del_name.is_empty() {
                        eprintln!("ERROR: 用法 dyj_cli record delete <name> [设备名]");
                        std::process::exit(2);
                    }
                    let name = args.get(3).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
                    with_device(name, move |device| async move {
                        device.record_delete(&del_name).await
                    })
                    .await
                }
                other => {
                    eprintln!("未知 record 子命令: {other}（list/delete）");
                    std::process::exit(2);
                }
            }
        }
        "upload" => {
            let sub = args.get(1).map(|s| s.as_str()).unwrap_or("status");
            match sub {
                "status" => {
                    let name = args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
                    with_device(name, |device| async move { device.upload_status().await }).await
                }
                "config" => {
                    let name = args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
                    with_device(
                        name,
                        |device| async move { device.upload_config_get().await },
                    )
                    .await
                }
                "set" => {
                    // url 传空串 = 清覆盖回落默认（协议语义）；缺参才是用法错误
                    let url = match args.get(2) {
                        Some(u) => u.clone(),
                        None => {
                            eprintln!("ERROR: 用法 dyj_cli upload set <url|\"\"> [token] [设备名]（url 传空串清覆盖）");
                            std::process::exit(2);
                        }
                    };
                    let token = args.get(3).map(|s| s.as_str());
                    let name = args
                        .get(if token.is_some() { 4 } else { 3 })
                        .map(|s| s.as_str())
                        .unwrap_or(DEFAULT_DEVICE);
                    with_device(name, move |device| async move {
                        device.upload_config_set(Some(&url), token).await
                    })
                    .await
                }
                "start" => {
                    let arg2 = args.get(2).cloned();
                    // start [name] [设备名]：第二参若以 DYJ_ 开头则视为设备名（缺省 name=全部未传）
                    let (file_name, name) = match arg2.as_deref() {
                        Some(n) if !n.starts_with("DYJ_") => (
                            Some(n.to_string()),
                            args.get(3)
                                .cloned()
                                .unwrap_or_else(|| DEFAULT_DEVICE.into()),
                        ),
                        Some(n) => (None, n.to_string()),
                        None => (None, DEFAULT_DEVICE.to_string()),
                    };
                    with_device(&name, move |device| async move {
                        device.upload_start(file_name.as_deref()).await
                    })
                    .await
                }
                other => {
                    eprintln!("未知 upload 子命令: {other}（status/config/set/start）");
                    std::process::exit(2);
                }
            }
        }
        "ota" => {
            let sub = args.get(1).map(|s| s.as_str()).unwrap_or("status");
            match sub {
                "check" => {
                    let name = args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
                    with_device(name, |device| async move { device.ota_check().await }).await
                }
                "install" => {
                    let name = args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
                    with_device(name, |device| async move { device.ota_install(true).await }).await
                }
                "status" => {
                    let name = args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
                    with_device(name, |device| async move { device.ota_status().await }).await
                }
                "config" => {
                    let name = args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
                    with_device(name, |device| async move { device.ota_config_get().await }).await
                }
                "set" => {
                    let url = args.get(2).cloned().unwrap_or_default();
                    if url.is_empty() {
                        eprintln!(
                            "ERROR: 用法 dyj_cli ota set <url|\"\"> [设备名]（url 传空串清覆盖）"
                        );
                        std::process::exit(2);
                    }
                    let name = args.get(3).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
                    with_device(name, move |device| async move {
                        device.ota_config_set(&url).await
                    })
                    .await
                }
                other => {
                    eprintln!("未知 ota 子命令: {other}（check/install/status/config/set）");
                    std::process::exit(2);
                }
            }
        }
        "wifi" => {
            let sub = args.get(1).map(|s| s.as_str()).unwrap_or("status");
            match sub {
                "scan" => {
                    let name = args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
                    with_device(name, |device| async move { device.wifi_scan().await }).await
                }
                "connect" => {
                    let ssid = args.get(2).cloned().unwrap_or_default();
                    if ssid.is_empty() {
                        eprintln!("ERROR: 用法 dyj_cli wifi connect <ssid> [password] [设备名]");
                        std::process::exit(2);
                    }
                    let password = args.get(3).map(|s| s.as_str());
                    let name = args.get(4).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
                    with_device(name, move |device| async move {
                        device.wifi_connect(&ssid, password, None).await
                    })
                    .await
                }
                "status" => {
                    let name = args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
                    with_device(name, |device| async move { device.wifi_status().await }).await
                }
                "forget" => {
                    let target = match args.get(2).map(|s| s.as_str()) {
                        None | Some("-") => None, // 缺省或 "-" = 删除全部
                        Some(s) => Some(s),
                    };
                    let name = args.get(3).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
                    with_device(name, move |device| async move {
                        device.wifi_forget(target).await
                    })
                    .await
                }
                other => {
                    eprintln!("未知 wifi 子命令: {other}（scan/connect/status/forget）");
                    std::process::exit(2);
                }
            }
        }
        "ap" => {
            let sub = args.get(1).map(|s| s.as_str()).unwrap_or("status");
            match sub {
                "start" => {
                    let positional: Vec<&String> = args
                        .iter()
                        .skip(2)
                        .filter(|a| a.as_str() != "--allow-sta-drop")
                        .collect();
                    let allow_sta_drop = args.iter().any(|a| a.as_str() == "--allow-sta-drop");
                    let ssid = positional.first().map(|s| s.as_str());
                    let password = positional.get(1).map(|s| s.as_str());
                    let name = positional
                        .get(2)
                        .map(|s| s.as_str())
                        .unwrap_or(DEFAULT_DEVICE);
                    with_device(name, move |device| async move {
                        device.ap_start(ssid, password, allow_sta_drop).await
                    })
                    .await
                }
                "stop" => {
                    let name = args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
                    with_device(name, |device| async move { device.ap_stop().await }).await
                }
                "status" => {
                    let name = args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
                    with_device(name, |device| async move { device.ap_status().await }).await
                }
                "files" => {
                    let name = args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
                    with_device(name, |device| async move {
                        let st = device.ap_status().await?;
                        let client = reai_dyj_sdk::ApClient::from_status(&st)?;
                        let list = client.list_files().await?;
                        Ok(serde_json::to_value(list).unwrap_or_default())
                    })
                    .await
                }
                "download" => {
                    let fname = args.get(2).cloned().unwrap_or_default();
                    let dir = args.get(3).cloned().unwrap_or_else(|| ".".into());
                    let name = args.get(4).map(|s| s.as_str()).unwrap_or(DEFAULT_DEVICE);
                    if fname.is_empty() {
                        eprintln!("ERROR: 用法 dyj_cli ap download <name> <目录> [设备名]");
                        std::process::exit(2);
                    }
                    with_device(name, move |device| async move {
                        let st = device.ap_status().await?;
                        let client = reai_dyj_sdk::ApClient::from_status(&st)?;
                        let saved = client.download_to_dir(&fname, &dir).await?;
                        Ok(serde_json::json!({
                            "file": saved.path,
                            "bytes": saved.bytes_written,
                            "md5": saved.md5,
                        }))
                    })
                    .await
                }
                "ota" => {
                    // AP 通道 OTA 上传（只暂存）：安装仍需 dyj_cli ota install（BLE 确认）
                    // host 可选 "ip[:port]" 覆盖 ap_status 上报的地址（调试转发场景）
                    let apk = args.get(2).cloned().unwrap_or_default();
                    if apk.is_empty() {
                        eprintln!("ERROR: 用法 dyj_cli ap ota <apk路径> [host[:port]] [设备名]（先 ap start 开热点）");
                        std::process::exit(2);
                    }
                    let host_arg = args.get(3).cloned().unwrap_or_default();
                    let name = args
                        .get(if host_arg.is_empty() { 3 } else { 4 })
                        .map(|s| s.as_str())
                        .unwrap_or(DEFAULT_DEVICE);
                    with_device(name, move |device| async move {
                        let st = device.ap_status().await?;
                        let client = match host_arg.split_once(':') {
                            Some((ip, port)) => reai_dyj_sdk::ApClient::new(
                                ip,
                                port.parse().map_err(|_| {
                                    reai_dyj_sdk::SdkError::InvalidParams(format!(
                                        "bad port: {port}"
                                    ))
                                })?,
                            )?,
                            None if host_arg.is_empty() => {
                                reai_dyj_sdk::ApClient::from_status(&st)?
                            }
                            None => reai_dyj_sdk::ApClient::new(
                                &host_arg,
                                reai_dyj_sdk::access_point::AP_HTTP_PORT,
                            )?,
                        };
                        let staged = client.upload_ota(&apk).await?;
                        Ok(serde_json::json!({
                            "staged": staged,
                            "next": "dyj_cli ota install（BLE 确认后安装）",
                        }))
                    })
                    .await
                }
                other => {
                    eprintln!("未知 ap 子命令: {other}（start/stop/status/files/download/ota）");
                    std::process::exit(2);
                }
            }
        }
        other => {
            eprintln!("未知子命令: {other}");
            std::process::exit(2);
        }
    };

    match result {
        Ok(out) => {
            println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        }
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(1);
        }
    }
}

async fn scan(prefix: &str) -> Result<Vec<serde_json::Value>, SdkError> {
    let manager = DeviceManager::new().await?;
    let devices = manager.scan(prefix, Duration::from_secs(5)).await?;
    Ok(devices
        .into_iter()
        .map(|d| serde_json::json!({"name": d.name, "rssi": d.rssi}))
        .collect())
}

/// 连接 → 开音频流 → 收 `seconds` 秒 → 解码 PCM 存 WAV（out 以 .amr 结尾则存原始帧流）。
/// RAII drop 自动 stop，随后显式断开。
async fn audio_capture(
    name: &str,
    seconds: u64,
    out_path: &str,
) -> Result<serde_json::Value, SdkError> {
    let manager = DeviceManager::new().await?;
    let device = manager.connect_by_name(name).await?;
    println!("[connected] {name}");

    let mut session = reai_dyj_sdk::RealtimeAudioSession::start(&device).await?;
    println!("[audio] streaming; capturing {seconds}s ...");

    let mut samples: Vec<i16> = Vec::new();
    let mut chunks = 0u64;
    let mut gaps = 0u64;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    loop {
        let chunk = match tokio::time::timeout_at(deadline, session.next_chunk()).await {
            Ok(Ok(chunk)) => chunk,
            Ok(Err(SdkError::AudioLagged { lost_chunks })) => {
                gaps += lost_chunks;
                continue;
            }
            Ok(Err(e)) => return Err(e),
            Err(_) => break, // 到时
        };
        if chunk.discontinuous {
            gaps += 1;
        }
        chunks += 1;
        samples.extend_from_slice(&chunk.samples);
    }
    drop(session); // RAII：drop 内部异步发 audio.stream.stop

    std::fs::write(out_path, reai_dyj_sdk::audio::wav_bytes(&samples, 16_000))
        .map_err(|e| SdkError::InvalidParams(format!("write {out_path}: {e}")))?;

    let _ = device.disconnect().await;
    Ok(serde_json::json!({
        "file": out_path,
        "chunks": chunks,
        "gaps": gaps,
        "samples": samples.len(),
        "duration_secs": (samples.len() as f64 / 16_000.0 * 1000.0).round() / 1000.0,
    }))
}

async fn with_device<F, Fut>(name: &str, f: F) -> Result<serde_json::Value, SdkError>
where
    F: FnOnce(reai_dyj_sdk::DyjDevice) -> Fut,
    Fut: std::future::Future<Output = Result<serde_json::Value, SdkError>>,
{
    let manager = DeviceManager::new().await?;
    let device = manager.connect_by_name(name).await?;
    println!("[connected] {name}");

    let mut events = device.subscribe_events();
    let event_task = tokio::spawn(async move {
        while let Ok(evt) = events.recv().await {
            match evt {
                reai_dyj_sdk::DeviceEvent::Event { evt, ts, data } => {
                    println!("[event] {evt} ts={ts} {data}");
                }
                reai_dyj_sdk::DeviceEvent::Disconnected => {
                    println!("[event] disconnected");
                    break;
                }
            }
        }
    });

    let out = f(device.clone()).await;
    // 必须显式断开：macOS CoreBluetooth 进程退出前 drop 对象不会断开链路，
    // 设备端收不到断连事件会一直保持"已连接"（不恢复广播），后续 CLI 全部连不上
    let _ = device.disconnect().await;
    event_task.abort();
    out
}
