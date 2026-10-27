//! LED 能力域验收（**单连接**内完成：协议面 / 超时回落 / 状态联动 / led.state 事件）。
//! （多命令用多个 CLI 进程会反复扫描/连接，在 SPRD 上间歇性超时——验收一律用本例。）
//!
//! 用法: cargo run -p reai-dyj-sdk --example led_check -- [设备名=DYJ_RA6BO5]
//!
//! 流程：hello(auto_audio=false，避免推流 claim 700 压住验收 pattern) →
//!   1) led.status 基线（BLE 已连 → 蓝常亮 claim 300 生效） →
//!   2) led.set solid_red/high 3s → 1s 后 status 确认 override 生效 → 3.5s 后 status 确认自动回落 →
//!   3) record.start → status 确认 record(600 白中亮) 压过 ble(300) → record.stop 回落 →
//!   4) led.clear（无 override 时 cleared 为空，幂等）。
//!
//! 全程打印 led.state 事件；实灯现象可目视同步核对。

use reai_dyj_sdk::{DeviceEvent, DeviceManager, DyjDevice, SdkError};
use std::time::Duration;

async fn show_status(device: &DyjDevice) -> Result<(), SdkError> {
    let s = device.led_status().await?;
    println!("[led.status] active={} claims={}", s["active"], s["claims"]);
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), SdkError> {
    let name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "DYJ_RA6BO5".into());
    let manager = DeviceManager::new().await?;
    let device = manager.connect_by_name(&name).await?;
    println!("[connected] {name}");

    // led.state 事件全程打印（生效灯效变化的实时流）
    let mut events = device.subscribe_events();
    let printer = tokio::spawn(async move {
        while let Ok(evt) = events.recv().await {
            if let DeviceEvent::Event { evt, ts, data } = evt {
                if evt == "led.state" {
                    println!("[led.state] ts={ts} {data}");
                }
            } else {
                break;
            }
        }
    });

    // auto_audio=false：验收只看 LED，避免推流 claim(700 白高亮) 压过验收 pattern
    let hello = device.hello("led_check", false).await?;
    println!(
        "[hello] app={} proto=v{}",
        hello["app_version"], hello["proto_version"]
    );

    // 1) 基线
    show_status(&device).await?;

    // 2) 临时覆盖 + 超时自动回落（红常亮·高亮 3s）
    let set = device
        .led_set("solid_red", Some("high"), None, Some(3))
        .await?;
    println!("[led.set] {set}（实灯：红常亮·高亮）");
    tokio::time::sleep(Duration::from_secs(1)).await;
    show_status(&device).await?; // override 生效中
    println!("[sleep 3.5s] 等待超时回落…");
    tokio::time::sleep(Duration::from_millis(3_500)).await;
    show_status(&device).await?; // 已回落（ble 蓝常亮）

    // 3) 录音联动（record 600 白中亮 压过 ble 300 蓝低亮）
    let started = device.record_start(Some(10), None).await?;
    println!(
        "[record.start] session={}（实灯：白常亮·中亮）",
        started["session"]
    );
    tokio::time::sleep(Duration::from_secs(1)).await;
    show_status(&device).await?;
    let stopped = device.record_stop().await?;
    println!("[record.stop] files_written={}", stopped["files_written"]);
    tokio::time::sleep(Duration::from_secs(1)).await;
    show_status(&device).await?;

    // 4) clear 幂等（当前无 override/fault，cleared 应为空）
    let cleared = device.led_clear().await?;
    println!("[led.clear] {cleared}");

    device.disconnect().await?;
    printer.abort();
    println!("[done] PASSED");
    Ok(())
}
