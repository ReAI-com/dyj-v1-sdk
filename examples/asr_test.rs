//! 端侧语音识别域验收（协议 §9.1 asr.*，asr 方案 2026-11-02）：**单连接内**完成状态查询 →
//! 开识别（首次加载模型 ~10s）→ 收 EVT asr.partial/asr.final 若干秒 → stop 回残余 → 断连。
//!
//! 用法: cargo run -p reai-dyj-sdk --example asr_test -- [设备名] [识别秒数=20] [once|stream|attach]
//! `stream`：同一连接先开 audio.stream.start（AMR 实时流）再识别——验证 PCM 旁路与 BLE 推流共存
//! `attach`：设 asr.config attach=true 后**只开实时流不手动 asr.start**——应自动出字；关流后 running 应为 false
//! 验收要点：asr.start 首次带 loaded_ms（~10000）与 buffered_ms（加载期间排队音频）；对设备说话
//! （或扬声器放 16k 中文音频）应收到 partial 增量与 final 整句；停止时未结句经 tail padding 刷出到
//! stop 的 text；同时开 `audio.stream.start` BLE 链路应不掉（PCM 旁路与 AMR 推流共存）。

use std::time::Duration;

use reai_dyj_sdk::{DeviceEvent, DeviceManager, SdkError};

#[tokio::main]
async fn main() -> Result<(), SdkError> {
    let name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "DYJ_RA6BO5".into());
    let watch_s: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let flag = std::env::args().nth(3).unwrap_or_default();
    let once = flag == "once";
    let attach = flag == "attach";
    let with_stream = flag == "stream" || attach;

    let manager = DeviceManager::new().await?;
    let device = manager.connect_by_name(&name).await?;
    println!("[connected] {name}");

    println!("[asr.status] {}", device.asr_status().await?);

    let mut events = device.subscribe_events();
    let mut audio = device.subscribe_audio();
    if attach {
        println!(
            "[asr.config.set] {}",
            device.asr_config_set(Some(true), Some(true)).await?
        );
    }
    if with_stream {
        println!(
            "[audio.stream.start] {}",
            device.audio_stream_start().await?
        );
    }
    if attach {
        // 不手动 start：attach 应已跟随推流自动开
        tokio::time::sleep(Duration::from_millis(800)).await;
        println!(
            "[asr.status after stream.start] {}",
            device.asr_status().await?
        );
    } else {
        let mode = if once { Some("once") } else { None };
        let started = device.asr_start(mode, Some(true), Some(600)).await?;
        println!("[asr.start] {started}");
    }
    println!("[asr] 请对设备说话（{watch_s}s）…");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(watch_s);
    let (mut partials, mut finals, mut chunks) = (0usize, 0usize, 0usize);
    loop {
        // 共存模式下顺带消费 FF12 音频块（只计数，证明推流没停）
        drain_audio(&mut audio, &mut chunks);
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Ok(DeviceEvent::Event { evt, ref data, .. })) if evt == "asr.partial" => {
                partials += 1;
                println!(
                    "[partial #{partials}] {}",
                    data["text"].as_str().unwrap_or("")
                );
            }
            Ok(Ok(DeviceEvent::Event { evt, ref data, .. })) if evt == "asr.final" => {
                finals += 1;
                println!("[final #{finals}] {data}");
                if once {
                    break;
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }

    if attach {
        drain_audio(&mut audio, &mut chunks);
        println!("[audio.stream.stop] {}", device.audio_stream_stop().await?);
        tokio::time::sleep(Duration::from_millis(800)).await;
        println!(
            "[asr.status after stream.stop] {}",
            device.asr_status().await?
        );
        println!("[coexist] AMR chunks received during ASR: {chunks}");
        println!("[summary] partials={partials} finals={finals}");
        let _ = device.disconnect().await;
        return Ok(());
    }

    let stopped = device.asr_stop().await?;
    println!("[asr.stop] {stopped}");
    if with_stream {
        drain_audio(&mut audio, &mut chunks);
        println!("[audio.stream.stop] {}", device.audio_stream_stop().await?);
        println!("[coexist] AMR chunks received during ASR: {chunks}");
    }
    println!("[asr.status] {}", device.asr_status().await?);
    println!("[summary] partials={partials} finals={finals}");

    let _ = device.disconnect().await;
    Ok(())
}

/// 清空音频广播通道并计数；Lagged 的丢块也计入（证明推流持续，不要求逐块消费）。
fn drain_audio(rx: &mut tokio::sync::broadcast::Receiver<reai_dyj_sdk::PcmChunk>, n: &mut usize) {
    loop {
        match rx.try_recv() {
            Ok(_) => *n += 1,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(k)) => *n += k as usize,
            Err(_) => break,
        }
    }
}
