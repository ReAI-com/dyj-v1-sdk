//! 系统控制域验收（协议 §9.1 vibrator / volume / device）：**单连接内**完成安全路径——
//! 震动（体感）→ 取消 → 音量回读 → 设值回读（越界 clamp）→ 时钟回读 → 对时偏差 →
//! reboot confirm 缺失拒绝（2003）。**真实重启不在本脚本**（断连终局，走
//! `dyj_cli device reboot` 单独验收）。
//!
//! 用法: cargo run -p reai-dyj-sdk --example sysctrl_test -- [设备名]

use reai_dyj_sdk::{DeviceManager, SdkError};

#[tokio::main]
async fn main() -> Result<(), SdkError> {
    let name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "DYJ_RA6BO5".into());

    let manager = DeviceManager::new().await?;
    let device = manager.connect_by_name(&name).await?;
    println!("[connected] {name}");

    // ---- 震动（拿在手里应有 300ms 体感）+ 取消 ----
    println!(
        "[vibrator.once 300ms] {}",
        device.vibrator_once(Some(300), None).await?
    );
    println!("[vibrator.cancel] {}", device.vibrator_cancel().await?);

    // ---- 音量：回读 → 设值（越界 99 应 clamp 到 max）→ 回读 ----
    let before = device.volume_get().await?;
    println!("[volume.get] {before}");
    let set = device.volume_set("music", 99).await?;
    println!("[volume.set music=99] {set}（current 应 = music.max）");
    let max = before["streams"]["music"]["max"].as_i64().unwrap_or(15);
    let _ = device.volume_set("music", max).await?; // 还原

    // ---- 对时：回读 → 设当前宿主时间 → 回读偏差应 < 1s ----
    let t0 = device.device_time(None).await?;
    println!("[device.time 读] {t0}");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let t1 = device.device_time(Some(now)).await?;
    let drift = t1["epoch_ms"].as_i64().unwrap_or(0) - now;
    println!(
        "[device.time 对时] applied={} 偏差={drift}ms（|偏差| 应 <1000）",
        t1["applied"]
    );

    // ---- reboot 安全路径：缺 confirm 必须被拒 2003 ----
    match device.device_reboot(false).await {
        Err(SdkError::Device { code: 2003, msg }) => {
            println!("[reboot confirm 缺失] code=2003 {msg} ✓")
        }
        other => println!("[reboot confirm 缺失] 预期 2003，实际 {other:?}"),
    }

    println!("[done] 真实重启单独走 dyj_cli device reboot 验收");
    device.disconnect().await?;
    Ok(())
}
