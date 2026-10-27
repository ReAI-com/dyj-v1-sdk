//! 批次 A（STA 配网）单连接验收脚本：一次 BLE 连接内跑完全部 wifi.* 验收项。
//!
//! 前置：设备 STA 已连网（脚本最后会验证恢复）；adb 可用（svc wifi 开关触发 conn.wifi 事件）。
//! 用法: cargo run --example wifi_sta [-- 设备名=DYJ_RA6BO5]
//!
//! 验收项（对应 issue #2 批次 A）：
//!   1. wifi.status 基线（ssid/ip）
//!   2. wifi.scan 流式列表（含生产网络，只列不连）
//!   3. wifi.forget 不存在的 ssid → removed:0（不碰生产配置）
//!   4. conn.wifi 事件：svc wifi disable → disconnected；enable → connected（3s 去抖语义）
//!   5. wifi.connect 负路径：连不存在的 ssid → connected:false + phase，自动清理恢复原网络
//!   6. 终态：wifi.status 恢复 ssid/ip（绝不能把设备留在断网状态）

use reai_dyj_sdk::{DeviceEvent, DeviceManager};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "DYJ_RA6BO5".into());

    let manager = DeviceManager::new().await?;
    let device = manager.connect_by_name(&name).await?;
    println!("[connected] {name}（单连接跑完全部验收项）");

    let mut events = device.subscribe_events();
    let mut task_events = device.subscribe_events();
    let event_task = tokio::spawn(async move {
        while let Ok(evt) = task_events.recv().await {
            if let DeviceEvent::Event { evt, ts, data } = evt {
                println!("[event] {evt} ts={ts} {data}");
            }
        }
    });

    let mut pass = 0u32;
    let mut fail = 0u32;
    macro_rules! check {
        ($name:expr, $cond:expr) => {
            if $cond {
                println!("✅ {}", $name);
                pass += 1;
            } else {
                println!("❌ {}", $name);
                fail += 1;
            }
        };
    }

    // 1. status 基线
    let st = device.wifi_status().await?;
    println!("[1] status: {st}");
    let baseline_ssid = st["ssid"].as_str().unwrap_or("").to_string();
    let baseline_ip = st["ip"].as_str().unwrap_or("").to_string();
    check!(
        "wifi.status 基线（wifi_enabled + ssid + ip）",
        st["wifi_enabled"].as_bool() == Some(true)
            && !baseline_ssid.is_empty()
            && !baseline_ip.is_empty()
    );

    // 2. scan（indexed 流式聚合；生产网络只列不连）
    let scan = device.wifi_scan().await?;
    let total = scan["total"].as_u64().unwrap_or(0);
    println!(
        "[2] scan: {total} 网络，{:?}（前3）",
        scan["items"]
            .as_array()
            .map(|a| a
                .iter()
                .take(3)
                .map(|i| i["ssid"].as_str().unwrap_or(""))
                .collect::<Vec<_>>())
            .unwrap_or_default()
    );
    check!(
        "wifi.scan 列出真热点（含目标网络）",
        total > 0
            && scan["items"]
                .as_array()
                .map(|a| a
                    .iter()
                    .any(|i| i["ssid"].as_str() == Some(baseline_ssid.as_str())))
                .unwrap_or(false)
    );

    // 3. forget 不存在的 ssid（不碰生产配置）
    let out = device.wifi_forget(Some("DYJ_NONEXIST_XYZ")).await?;
    println!("[3] forget 不存在 ssid: {out}");
    check!(
        "wifi.forget 不存在的 ssid → removed:0",
        out["removed"].as_i64() == Some(0)
    );

    // 4. conn.wifi 事件（svc wifi 开关触发；3s 去抖 + 状态变化才发）
    adb(&["-s", "0123456789ABCDEF", "shell", "svc", "wifi", "disable"]);
    println!("[4] svc wifi disable 已发，等 disconnected 事件…");
    let got_disc = wait_event(&mut events, "conn.wifi", "disconnected", 20).await;
    adb(&["-s", "0123456789ABCDEF", "shell", "svc", "wifi", "enable"]);
    println!("[4] svc wifi enable 已发，等 connected 事件（系统自动重连）…");
    let got_conn = wait_event(&mut events, "conn.wifi", "connected", 30).await;
    check!("conn.wifi disconnected 事件（去抖后单发）", got_disc);
    check!("conn.wifi connected 事件（自动重连后）", got_conn);

    // 5. 负路径连接：不存在的 ssid → 超时/auth 失败 + 自动清理
    println!("[5] wifi.connect 不存在的 ssid（约 30-35s，设备会短暂离网后自动恢复）…");
    let t1 = std::time::Instant::now();
    let out = device
        .wifi_connect("DYJ_TEST_NO_SUCH_SSID", Some("wrongpass"), None)
        .await?;
    let elapsed = t1.elapsed();
    println!("[5] connect 负路径: {out}（耗时 {elapsed:?}）");
    let phase = out["phase"].as_str().unwrap_or("");
    check!(
        "wifi.connect 负路径 → connected:false + phase",
        out["connected"].as_bool() == Some(false) && (phase == "timeout" || phase == "auth")
    );

    // 6. 终态恢复
    let st = device.wifi_status().await?;
    println!("[6] 终态 status: {st}");
    check!(
        "连接失败后恢复原网络（ssid + ip 回到基线）",
        st["ssid"].as_str() == Some(baseline_ssid.as_str())
            && !st["ip"].as_str().unwrap_or("").is_empty()
    );

    let _ = device.disconnect().await;
    event_task.abort();
    println!("\n===== 批次 A 验收: {pass} 过 / {fail} 败 =====");
    if fail > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// adb 调用：优先 PATH，回退 Android SDK 默认位置（脚本环境常不带 platform-tools）。
fn adb(args: &[&str]) {
    let candidates = [
        "adb",
        concat!(env!("HOME"), "/Library/Android/sdk/platform-tools/adb"),
    ];
    for bin in candidates {
        if let Ok(status) = std::process::Command::new(bin).args(args).status() {
            if status.success() {
                return;
            }
        }
    }
    eprintln!("⚠️ adb 调用失败（conn.wifi 事件项会超时）: {args:?}");
}

/// 等待指定事件（evt 名 + data.state 匹配），超时秒数内未到返回 false。
async fn wait_event(
    events: &mut tokio::sync::broadcast::Receiver<DeviceEvent>,
    evt_name: &str,
    state: &str,
    timeout_secs: u64,
) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Ok(DeviceEvent::Event { evt, data, .. })) => {
                if evt == evt_name && data["state"].as_str() == Some(state) {
                    println!("[event ✓] {evt} state={state}");
                    return true;
                }
            }
            Ok(Ok(DeviceEvent::Disconnected)) | Ok(Err(_)) => return false,
            Err(_) => return false, // 超时
        }
    }
}
