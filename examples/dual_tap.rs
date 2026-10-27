//! 双 tap 并发验收：**单连接内**完成 录音开始 → 录音期间实时流 → 状态 → 停止。
//! （多命令用多个 CLI 进程会反复扫描/连接，在 SPRD 上间歇性超时——验收一律用本例。）
//!
//! 用法: cargo run -p reai-dyj-sdk --example dual_tap -- [设备名=DYJ_RA6BO5] [segment_min] [stream_secs]
//! 例：`dual_tap DYJ_RA6BO5 1 70` 用 1 分钟分段验证滚动 seq 递增。

use reai_dyj_sdk::{DeviceManager, RealtimeAudioSession, SdkError};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), SdkError> {
    let name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "DYJ_RA6BO5".into());
    let segment_min = std::env::args().nth(2).and_then(|s| s.parse().ok());
    let stream_secs: u64 = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let manager = DeviceManager::new().await?;
    let device = manager.connect_by_name(&name).await?;
    println!("[connected] {name}");

    let started = device.record_start(segment_min, None).await?;
    println!("[record.start] {started}");

    tokio::time::sleep(Duration::from_secs(2)).await;
    let mut session = RealtimeAudioSession::start(&device).await?;
    let mut samples = 0usize;
    let mut gaps = 0u64;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(stream_secs);
    loop {
        match tokio::time::timeout_at(deadline, session.next_chunk()).await {
            Ok(Ok(chunk)) => {
                samples += chunk.samples.len();
                if chunk.discontinuous {
                    gaps += 1;
                }
            }
            Ok(Err(SdkError::AudioLagged { lost_chunks })) => gaps += lost_chunks,
            Ok(Err(e)) => return Err(e),
            Err(_) => break,
        }
    }
    drop(session); // RAII 停流（录音不受影响）
    println!(
        "[stream while recording] samples={samples} gaps={gaps} secs={:.2}",
        samples as f64 / 16_000.0
    );

    let st = device.record_status().await?;
    println!("[record.status] {st}");

    let stopped = device.record_stop().await?;
    println!("[record.stop] {stopped}");

    let list = device.record_list().await?;
    println!("[record.list] total={}", list["total"]);
    for item in list["items"].as_array().unwrap_or(&vec![]) {
        println!(
            "  - {} session={:?} seq={:?}",
            item["name"], item["session"], item["seq"]
        );
    }

    device.disconnect().await?;
    println!("[done] PASSED");
    Ok(())
}
