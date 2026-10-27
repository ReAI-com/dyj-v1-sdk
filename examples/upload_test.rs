//! 上传能力域验收（协议 §9.1 record.delete/upload.*，§9.2 upload.state）：**单连接内**完成
//! 配置回读 →（可选覆盖 endpoint）→ 触发上传 → 订阅 upload.state 事件流 → 状态汇总 →
//! （可选）删除验证 → 恢复配置。
//!
//! 用法: cargo run -p reai-dyj-sdk --example upload_test -- [设备名] [监听秒数=60] [url|"-"] [delete_name|"-"]
//! 例：`upload_test DYJ_RA6BO5 60 - -`            只触发全部未传并观察事件
//!     `upload_test DYJ_RA6BO5 60 https://x/u -`  先覆盖 endpoint 再上传（结束恢复默认）
//!     `upload_test DYJ_RA6BO5 60 - <name>`       结束前删除指定分段（record.delete 验证）

use reai_dyj_sdk::{DeviceEvent, DeviceManager, SdkError};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), SdkError> {
    let name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "DYJ_RA6BO5".into());
    let listen_secs: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let url_arg = std::env::args().nth(3).unwrap_or_else(|| "-".into());
    let delete_name = std::env::args().nth(4).filter(|s| s != "-");

    let manager = DeviceManager::new().await?;
    let device = manager.connect_by_name(&name).await?;
    println!("[connected] {name}");

    println!("[upload.config.get] {}", device.upload_config_get().await?);

    let restore_url = if url_arg != "-" {
        device.upload_config_set(Some(&url_arg), None).await?;
        println!("[upload.config.set] {url_arg}");
        true
    } else {
        false
    };

    let started = device.upload_start(None).await?;
    println!("[upload.start] {started}");

    let mut events = device.subscribe_events();
    println!("[listening {listen_secs}s] upload.state 事件：");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(listen_secs);
    let mut count = 0usize;
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Ok(DeviceEvent::Event { evt, ts, data })) if evt == "upload.state" => {
                count += 1;
                println!("  [upload.state] ts={ts} {}", data);
            }
            Ok(Ok(DeviceEvent::Event { .. })) => {}
            Ok(Ok(DeviceEvent::Disconnected)) => {
                println!("[disconnected] 设备断开，提前结束");
                break;
            }
            Ok(Err(_)) => {}
            Err(_) => break, // 到时
        }
    }
    println!("[events received] {count}");

    println!("[upload.status] {}", device.upload_status().await?);

    if let Some(del) = delete_name {
        println!(
            "[record.delete {del}] {}",
            device.record_delete(&del).await?
        );
    }

    if restore_url {
        device.upload_config_set(Some(""), None).await?;
        println!("[restored] upload url -> default");
    }

    device.disconnect().await?;
    println!("[done] PASSED");
    Ok(())
}
