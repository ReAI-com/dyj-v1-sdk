//! 按键能力域验收（协议 §9.1 button.* / §9.2 button.event）：**单连接内**完成
//! 绑定设置 → 订阅 button.event（注入/实按期间实时打印 type+duration_ms）→
//! 状态/绑定读回 → 恢复默认绑定。
//! （多 CLI 进程反复扫描连接在 SPRD 上间歇性超时——验收一律用本例单连接模式。）
//!
//! 用法: cargo run -p reai-dyj-sdk --example button_test -- [设备名=DYJ_RA6BO5] [监听秒数=30] [长按绑定动作]
//! 例：`button_test DYJ_RA6BO5 30 record.toggle` 先把 long_press 绑到 record.toggle，
//!     监听 30s 内在设备上按键（或 adb shell input keyevent KEYCODE_BACK [--longpress]），
//!     结束后恢复 event_only。录音启停由外部 record.status 验证（本例只管事件与绑定）。

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
        .unwrap_or(30);
    let bind_action = std::env::args()
        .nth(3)
        .unwrap_or_else(|| "record.toggle".into());

    let manager = DeviceManager::new().await?;
    let device = manager.connect_by_name(&name).await?;
    println!("[connected] {name}");

    let status = device.button_status().await?;
    println!("[button.status] {status}");

    // 长按 → 指定动作（默认 record.toggle），其余保持默认
    device.button_bind_set("long_press", &bind_action).await?;
    let bindings = device.button_bind_get().await?;
    println!("[bind.set long_press -> {bind_action}] {bindings}");

    // 订阅 button.event：注入/实按期间实时打印
    let mut events = device.subscribe_events();
    println!("[listening {listen_secs}s] 按设备物理键（或 adb 注入），事件实时打印：");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(listen_secs);
    let mut count = 0usize;
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Ok(DeviceEvent::Event { evt, ts, data })) if evt == "button.event" => {
                count += 1;
                println!(
                    "  [button.event] ts={ts} type={} duration_ms={:?}",
                    data["type"],
                    data.get("duration_ms")
                );
            }
            Ok(Ok(DeviceEvent::Event { .. })) => {} // 其他事件（led.state 等）忽略
            Ok(Ok(DeviceEvent::Disconnected)) => {
                println!("[disconnected] 设备断开，提前结束监听");
                break;
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(n))) => {
                println!("  [warn] 事件通道积压丢弃 {n} 条");
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                println!("[event channel closed]");
                break;
            }
            Err(_) => break, // 到时
        }
    }
    println!("[events received] {count}");

    // 恢复默认绑定
    device.button_bind_set("long_press", "event_only").await?;
    println!("[restored] long_press -> event_only");
    let final_status = device.button_status().await?;
    println!("[button.status] {final_status}");

    device.disconnect().await?;
    println!("[done] PASSED");
    Ok(())
}
