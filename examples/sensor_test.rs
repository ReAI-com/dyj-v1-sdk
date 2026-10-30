//! 传感器能力域验收（协议 §9.1 sensor.*）：**单连接内**完成能力查询 → 全类型实读 →
//! 子集实读 → 限流订阅（8s 收 EVT sensor.data 验证节流帧率）→ 停订阅 → 桩类型拒绝 → 断连。
//!
//! 用法: cargo run -p reai-dyj-sdk --example sensor_test -- [设备名] [订阅秒数=8]
//! 验收要点：气压 ~1013hPa 量级、地磁 ~50μT 量级；订阅帧率 ≈ 1/interval；
//! `accelerometer`（桩）报 1002；断连设备端自动停流。

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
        .unwrap_or(8);

    let manager = DeviceManager::new().await?;
    let device = manager.connect_by_name(&name).await?;
    println!("[connected] {name}");

    // ---- 能力查询（available 应为 5 类） ----
    let status = device.sensor_status().await?;
    println!("[sensor.status] {status}");

    // ---- 全类型单次实读（量级合理性人工判读） ----
    println!("[sensor.read all] {}", device.sensor_read(None).await?);

    // ---- 子集实读（仅气压） ----
    println!(
        "[sensor.read pressure] {}",
        device.sensor_read(Some(&["pressure"])).await?
    );

    // ---- 桩类型拒绝（1002） ----
    match device.sensor_read(Some(&["accelerometer"])).await {
        Err(SdkError::Device { code, msg }) => println!("[stub rejected] code={code} {msg}"),
        other => println!("[stub rejected] 预期 1002，实际 {other:?}"),
    }

    // ---- 限流订阅：500ms 节流，收 {watch_s}s 数帧 ----
    let mut events = device.subscribe_events();
    let started = device.sensor_stream_start(None, Some(500)).await?;
    println!("[sensor.stream.start] {started}");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(watch_s);
    let mut frames = 0usize;
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Ok(DeviceEvent::Event { evt, ref data, .. })) if evt == "sensor.data" => {
                frames += 1;
                println!("[sensor.data #{frames}] {data}");
            }
            Ok(Ok(_)) => {}      // 其他事件不关心
            Ok(Err(_)) => break, // 事件总线关闭
            Err(_) => break,     // 到时
        }
    }

    println!(
        "[sensor.stream.stop] {}",
        device.sensor_stream_stop().await?
    );
    println!("[done] frames={frames}");
    device.disconnect().await?;
    Ok(())
}
