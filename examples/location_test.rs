//! 定位能力域验收（协议 §9.1 location.*）：**单连接内**完成状态查询 → 一次取定位
//! （无 fix 正常返回）→ 订阅（可设窗边实测 TTFF）→ 停订阅 → 断连。
//!
//! 用法: cargo run -p reai-dyj-sdk --example location_test -- [设备名] [get超时s=25] [订阅秒数=20]
//! 验收要点：室内无 fix 正常返回 `{has_fix:false}`；窗边记录一次 TTFF 基线；
//! 订阅仅新 fix 才发 EVT location.fix；断连设备端自动停流。

use std::time::Duration;

use reai_dyj_sdk::{DeviceEvent, DeviceManager, SdkError};

#[tokio::main]
async fn main() -> Result<(), SdkError> {
    let name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "DYJ_RA6BO5".into());
    let get_timeout_s: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(25);
    let watch_s: u64 = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);

    let manager = DeviceManager::new().await?;
    let device = manager.connect_by_name(&name).await?;
    println!("[connected] {name}");

    // ---- 状态（enabled/gps_active/lastKnown 新鲜度） ----
    println!("[location.status] {}", device.location_status().await?);

    // ---- 一次取定位：无 fix 是正常业务结果（30s 命令超时覆盖 25s 参数上限） ----
    let t0 = std::time::Instant::now();
    let fix = device.location_get(Some(get_timeout_s)).await?;
    println!(
        "[location.get timeout={get_timeout_s}s elapsed={:.1}s] {fix}",
        t0.elapsed().as_secs_f32()
    );

    // ---- 订阅：仅新 fix 才发 EVT ----
    let mut events = device.subscribe_events();
    let started = device.location_stream_start(Some(5)).await?;
    println!("[location.stream.start] {started}");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(watch_s);
    let mut fixes = 0usize;
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Ok(DeviceEvent::Event { evt, ref data, .. })) if evt == "location.fix" => {
                fixes += 1;
                if fixes == 1 {
                    println!(
                        "[location.fix #1 TTFF≈{:.1}s] {data}",
                        t0.elapsed().as_secs_f32()
                    );
                } else {
                    println!("[location.fix #{fixes}] {data}");
                }
            }
            Ok(Ok(_)) => {}      // 其他事件不关心
            Ok(Err(_)) => break, // 事件总线关闭
            Err(_) => break,     // 到时
        }
    }

    println!(
        "[location.stream.stop] {}",
        device.location_stream_stop().await?
    );
    println!("[done] fixes={fixes}");
    device.disconnect().await?;
    Ok(())
}
