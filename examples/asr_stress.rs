//! 端侧 ASR 稳定性压力验收（三开：AMR 实时流 + 本地录音分段 + attach ASR 自动识别）。
//! 验证长会话下 BLE 链路是否存活（此前复现 reason=0x0008 监督超时，attach partial 已降 1/s）。
//!
//! 用法: cargo run -p reai-dyj-sdk --example asr_stress -- [设备名] [分钟=15] [segment_min=2]
//! 心跳每 60s 打一行：elapsed / partials / finals / AMR chunks / dropped_frames / segments；
//! 说话刺激由外部脚本 afplay 循环提供。任何 Disconnected 即打印并退出码 2。

use std::time::Duration;

use reai_dyj_sdk::{DeviceEvent, DeviceManager, SdkError};

#[tokio::main]
async fn main() -> Result<(), SdkError> {
    let name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "DYJ_RA6BO5".into());
    let minutes: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(15);
    let segment_min: u64 = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);

    let manager = DeviceManager::new().await?;
    let device = manager.connect_by_name(&name).await?;
    println!("[connected] {name}  plan={minutes}min segment_min={segment_min}");

    println!("[asr.status] {}", device.asr_status().await?);
    println!(
        "[asr.config.set] {}",
        device.asr_config_set(Some(true), Some(true)).await?
    );
    println!(
        "[record.start] {}",
        device.record_start(Some(segment_min), None).await?
    );
    println!(
        "[audio.stream.start] {}",
        device.audio_stream_start().await?
    );
    tokio::time::sleep(Duration::from_millis(800)).await;
    let st = device.asr_status().await?;
    println!("[asr after stream.start] {st}");

    let mut events = device.subscribe_events();
    let mut audio = device.subscribe_audio();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(minutes * 60);
    let started = tokio::time::Instant::now();
    let (mut partials, mut finals, mut chunks, mut segments) = (0usize, 0usize, 0usize, 0usize);
    let mut disconnected = false;
    let mut last_hb = started;

    loop {
        while let Ok(_) = audio.try_recv() {
            chunks += 1;
        }
        let got = tokio::time::timeout(Duration::from_secs(1), events.recv()).await;
        match got {
            Err(_) => {} // 1s 无事件：正常（静音期）
            Ok(Err(_)) => break,
            Ok(Ok(DeviceEvent::Disconnected)) => {
                println!(
                    "[!!] Disconnected at {}s",
                    (tokio::time::Instant::now() - started).as_secs()
                );
                disconnected = true;
                break;
            }
            Ok(Ok(DeviceEvent::Event { evt, ref data, .. })) => match evt.as_str() {
                "asr.partial" => partials += 1,
                "asr.final" => {
                    finals += 1;
                    println!(
                        "[final #{finals} @{}s] {}",
                        (tokio::time::Instant::now() - started).as_secs(),
                        data["text"].as_str().unwrap_or("")
                    );
                }
                "record.segment" => {
                    segments += 1;
                    println!("[segment #{segments}] {data}");
                }
                _ => {}
            },
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        if now.duration_since(last_hb) >= Duration::from_secs(60) {
            last_hb = now;
            let secs = (now - started).as_secs();
            let st = device.asr_status().await?;
            let dropped = st["dropped_frames"].as_u64().unwrap_or(0);
            let cps = chunks as f64 / secs.max(1) as f64;
            println!(
                "[heartbeat {secs}s] partials={partials} finals={finals} chunks={chunks} (~{cps:.1}/s) segments={segments} dropped={dropped} running={} attached={}",
                st["running"].as_bool().unwrap_or(false),
                st["attached"].as_bool().unwrap_or(false),
            );
        }
    }

    drain_audio(&mut audio, &mut chunks);
    let (stop_res, rec_res) = if disconnected {
        (String::from("skipped"), String::from("skipped"))
    } else {
        (
            format!("[asr.status] {}", device.asr_status().await?),
            format!(
                "[record.stop] {}\n[audio.stream.stop] {}",
                device.record_stop().await?,
                device.audio_stream_stop().await?
            ),
        )
    };
    println!("{stop_res}\n{rec_res}");
    println!(
        "[summary] partials={partials} finals={finals} chunks={chunks} segments={segments} disconnected={disconnected}"
    );

    let _ = device.disconnect().await;
    if disconnected {
        std::process::exit(2);
    }
    Ok(())
}

/// 清空音频广播通道并计数（Lagged 也计入）。
fn drain_audio(
    rx: &mut tokio::sync::broadcast::Receiver<reai_dyj_sdk::PcmChunk>,
    total: &mut usize,
) {
    loop {
        match rx.try_recv() {
            Ok(_) => *total += 1,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(k)) => *total += k as usize,
            Err(_) => break,
        }
    }
}
