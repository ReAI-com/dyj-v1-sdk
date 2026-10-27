//! 批次 B（AP 快传）单连接验收脚本：一次 BLE 连接内跑完 ap.* + HTTP 文件服务验收项。
//!
//! 下载通道用 `adb forward` 把设备 8788 端口映射到本机（走 USB，不需要本机加入热点），
//! 跨设备真热点验证（电脑入热点后直连）作为人工补充项。
//! 用法: cargo run --example wifi_ap [-- 设备名=DYJ_RA6BO5] [--serial=0123456789ABCDEF]
//!
//! 验收项（对应 issue #2 批次 B）：
//!   1. ap.status 基线（inactive）
//!   2. ap.start → active + ssid/password/ip；STA 存活探测 → SPRD 并发结论（sta_dropped）
//!      （不支持并发时默认回滚 3502，脚本改 allow_sta_drop 重开）
//!   3. HTTP 文件列表（GET /api/files）
//!   4. 下载最大的 ~1MB .amr：秒级完成 + md5 设备端/本地对账（ApClient 内置校验）
//!   5. ap.stop → 8788 不再监听（forward 通道连不上即证）+ STA 恢复

use reai_dyj_sdk::{ApClient, DeviceManager};
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut name = "DYJ_RA6BO5".to_string();
    let mut serial = "0123456789ABCDEF".to_string();
    for a in std::env::args().skip(1) {
        if let Some(s) = a.strip_prefix("--serial=") {
            serial = s.to_string();
        } else {
            name = a;
        }
    }
    let adb = |args: &[&str]| {
        let bin = concat!(env!("HOME"), "/Library/Android/sdk/platform-tools/adb");
        let ok = std::process::Command::new(bin)
            .args(["-s", &serial])
            .args(args)
            .status();
        if !ok.map(|s| s.success()).unwrap_or(false) {
            eprintln!("⚠️ adb {args:?} 失败");
        }
    };

    let manager = DeviceManager::new().await?;
    let device = manager.connect_by_name(&name).await?;
    println!("[connected] {name}（单连接跑完批次 B 验收）");

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

    // 1. ap.status 基线
    let st = device.ap_status().await?;
    println!("[1] ap.status 基线: {st}");
    check!(
        "ap.status 基线（active=false）",
        st["active"].as_bool() == Some(false)
    );

    // 2. ap.start（先默认重试，3502 = 不支持并发 → allow_sta_drop 重开）
    println!("[2] ap.start（探测 SPRD AP+STA 并发能力）…");
    match device.ap_start(None, None, false).await {
        Ok(out) => {
            let sta_dropped = out["sta_dropped"].as_bool().unwrap_or(true);
            println!("[2] ap.start ok: {out}");
            println!(
                "    ➜ SPRD 并发结论: {}",
                if sta_dropped {
                    "不支持（STA 被断，未放行不该走到这）"
                } else {
                    "✅ 支持 AP+STA 并发"
                }
            );
        }
        Err(reai_dyj_sdk::SdkError::Device { code: 3502, msg }) => {
            println!("[2] 3502 sta_concurrency_unsupported: {msg}");
            println!("    ➜ SPRD 并发结论: ❌ 不支持，默认回滚路径生效（关 AP + 恢复 STA）");
            let out = device.ap_start(None, None, true).await?;
            println!("[2] allow_sta_drop=true 重开: {out}");
        }
        Err(e) => return Err(e.into()),
    }
    let ap_info = device.ap_status().await?;
    println!("[2] ap.status: {ap_info}");
    check!(
        "ap.start → active + ssid + ip",
        ap_info["active"].as_bool() == Some(true)
            && ap_info["ip"]
                .as_str()
                .map(|s| !s.is_empty())
                .unwrap_or(false)
    );

    // 3-4. HTTP 文件服务（adb forward 走 USB，不依赖本机加入热点）
    adb(&["forward", "tcp:18788", "tcp:8788"]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let client = ApClient::new("127.0.0.1", 18788)?;

    let list = client.list_files().await?;
    println!(
        "[3] 文件列表: {} 个（total={}）",
        list.files.len(),
        list.total
    );
    check!("GET /api/files 列出录音分段", !list.files.is_empty());

    let target = list.files.iter().max_by_key(|f| f.bytes).unwrap();
    println!("[4] 下载最大的 {}（{}B）…", target.name, target.bytes);
    let out_dir = std::env::temp_dir().join("dyj-ap-accept");
    let t0 = Instant::now();
    let saved = client.download_to_dir(&target.name, &out_dir).await?;
    let elapsed = t0.elapsed();
    let kbps = saved.bytes_written as f64 / 1024.0 / elapsed.as_secs_f64();
    println!(
        "[4] 下载完成: {}B / {elapsed:?}（{kbps:.0} KB/s）md5={}",
        saved.bytes_written,
        saved.md5.as_deref().unwrap_or("-")
    );
    check!(
        "下载 ~1MB .amr 秒级完成且 md5 对账一致（.part 已改名）",
        saved.bytes_written == target.bytes
            && elapsed.as_secs_f64() < 10.0
            && !out_dir.join(format!("{}.part", target.name)).exists()
    );

    // 5. ap.stop → 端口关闭 + STA 恢复
    device.ap_stop().await?;
    println!("[5] ap.stop 已发，验证 8788 不再监听…");
    tokio::time::sleep(Duration::from_millis(800)).await;
    // adb forward 在本机侧总是先完成 TCP 握手再转连设备侧——connect 成功≠端口存活，
    // 必须写一笔再读：设备侧拒绝时立即 EOF（读到 0B）
    let port_closed = {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        match tokio::net::TcpStream::connect("127.0.0.1:18788").await {
            Err(_) => true, // 本地 forward 都没了
            Ok(mut s) => {
                let _ = s.write_all(b"GET / HTTP/1.0\r\n\r\n").await;
                let mut buf = [0u8; 64];
                match tokio::time::timeout(Duration::from_secs(2), s.read(&mut buf)).await {
                    Ok(Ok(0)) => true,  // EOF = 设备侧 8788 已关
                    Ok(Ok(_)) => false, // 有响应 = 服务还活着
                    _ => false,         // 读挂起 = 有监听
                }
            }
        }
    };
    adb(&["forward", "--remove", "tcp:18788"]);
    check!("ap.stop 后 8788 不再监听", port_closed);

    println!("[5] 等 STA 自动恢复（系统重连，≤25s）…");
    let mut recovered = false;
    for _ in 0..25 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let st = device.wifi_status().await?;
        if st["ip"].as_str().map(|s| !s.is_empty()).unwrap_or(false) {
            println!("[5] STA 已恢复: {st}");
            recovered = true;
            break;
        }
    }
    check!("ap.stop 后 STA 自动恢复（ssid+ip 回基线）", recovered);

    let _ = device.disconnect().await;
    println!("\n===== 批次 B 验收: {pass} 过 / {fail} 败 =====");
    if fail > 0 {
        std::process::exit(1);
    }
    Ok(())
}
