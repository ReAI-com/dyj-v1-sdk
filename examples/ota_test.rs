//! OTA 能力域验收（协议 §9.1 ota.*，§9.2 ota.progress）：**单连接内**完成
//! 清单 URL 配置（可选，指向本地 http.server 即可全链路验收）→ check → install(confirm)
//! → 订阅 ota.progress 直到 installing/failed。installing 后设备进程被覆盖安装杀死、
//! 自拉起回新版本——`DeviceEvent::Disconnected` 属预期，重连后 hello 回新版本号即验收。
//!
//! 用法: cargo run -p reai-dyj-sdk --example ota_test -- [设备名] [清单url|"-"]
//! 例：`ota_test DYJ_RA6BO5 -`                          只 check 不安装
//!     `ota_test DYJ_RA6BO5 http://127.0.0.1:8080/info-app.json`  覆盖清单并安装
//!     （配 adb reverse tcp:8080 + 本机静态目录；结束把清单 URL 恢复默认需另跑 `ota set ""`）

use reai_dyj_sdk::{DeviceEvent, DeviceManager, SdkError};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), SdkError> {
    let name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "DYJ_RA6BO5".into());
    let url_arg = std::env::args().nth(2).unwrap_or_else(|| "-".into());

    let manager = DeviceManager::new().await?;
    let device = manager.connect_by_name(&name).await?;
    println!("[connected] {name}");

    println!("[ota.config.get] {}", device.ota_config_get().await?);
    if url_arg != "-" {
        device.ota_config_set(&url_arg).await?;
        println!("[ota.config.set] {url_arg}");
    }

    let check = device.ota_check().await?;
    println!("[ota.check] {check}");

    if check["need_update"].as_bool() != Some(true) {
        println!(
            "[skip] need_update=false（升级/force 才安装）；如需重装当前版本，清单设 force:true"
        );
        device.disconnect().await?;
        return Ok(());
    }

    device.ota_install(true).await?;
    println!("[ota.install] accepted；等待 ota.progress …");

    let mut events = device.subscribe_events();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Ok(DeviceEvent::Event { evt, ts, data })) if evt == "ota.progress" => {
                println!("  [ota.progress] ts={ts} {}", data);
                let state = data["state"].as_str().unwrap_or("");
                if state == "installing" || state == "failed" {
                    break;
                }
            }
            Ok(Ok(DeviceEvent::Event { .. })) => {}
            Ok(Ok(DeviceEvent::Disconnected)) => {
                // installing 后 pm install 杀死进程 → 断连属预期；重连 hello 回新版本即验收
                println!("[disconnected] 预期内（覆盖安装杀进程）；等设备自拉起后用 hello/ota.status 复核版本");
                return Ok(());
            }
            Ok(Err(_)) => {}
            Err(_) => {
                println!("[timeout] 10 分钟无进展，退出");
                break;
            }
        }
    }

    device.disconnect().await?;
    println!("[done] PASSED");
    Ok(())
}
