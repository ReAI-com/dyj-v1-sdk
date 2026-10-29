//! 相机能力域验收（协议 §9.1 camera.*）：**单连接内**完成状态查询 → 拍照 → 列表 →
//! 删除（可选保留）→ 录像 start/status/stop → 列表汇总 → 断连。
//!
//! 用法: cargo run -p reai-dyj-sdk --example camera_test -- [设备名] [keep_name|"-"]
//! 例：`camera_test DYJ_RA6BO5 -`                  全流程跑完并把产物删干净
//!     `camera_test DYJ_RA6BO5 img_20261029-...`  保留指定文件（随后 upload_test 上传验证）

use reai_dyj_sdk::{DeviceManager, SdkError};

#[tokio::main]
async fn main() -> Result<(), SdkError> {
    let name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "DYJ_RA6BO5".into());
    let keep = std::env::args().nth(2).filter(|s| s != "-");

    let manager = DeviceManager::new().await?;
    let device = manager.connect_by_name(&name).await?;
    println!("[connected] {name}");

    println!("[camera.status] {}", device.camera_status().await?);
    println!(
        "[camera.record.status] {}",
        device.camera_record_status().await?
    );

    // ---- 拍照 → 列表 → （可选删除） ----
    let shot = device.camera_capture().await?;
    println!("[camera.capture] {shot}");
    println!("[camera.list] {}", device.camera_list().await?);
    let shot_name = shot["name"].as_str().unwrap_or_default().to_string();
    if keep.as_deref() != Some(shot_name.as_str()) {
        println!(
            "[camera.delete {shot_name}] {}",
            device.camera_delete(&shot_name).await?
        );
    }

    // ---- 短录像（10s 上限，实际 3s 停） ----
    let rec = device.camera_record_start(Some(10)).await?;
    println!("[camera.record.start] {rec}");
    std::thread::sleep(std::time::Duration::from_secs(3));
    println!(
        "[camera.record.status] {}",
        device.camera_record_status().await?
    );
    let stopped = device.camera_record_stop().await?;
    println!("[camera.record.stop] {stopped}");
    let rec_name = stopped["name"].as_str().unwrap_or_default().to_string();
    if keep.as_deref() != Some(rec_name.as_str()) && !rec_name.is_empty() {
        println!(
            "[camera.delete {rec_name}] {}",
            device.camera_delete(&rec_name).await?
        );
    }

    println!("[camera.status] {}", device.camera_status().await?);
    println!("[done] 保持的文件（keep）：{:?}", keep);
    device.disconnect().await?;
    Ok(())
}
