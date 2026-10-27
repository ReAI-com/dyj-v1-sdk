//! btleplug BLE 传输层（协议规则 §2：NUS 服务 + FF11 命令特征）。

use btleplug::api::{Central, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Adapter, Peripheral, PeripheralId};
use futures::stream::StreamExt;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::error::{Result, SdkError};

pub const SERVICE_UUID: &str = "6e400001-b5a3-f393-e0a9-e50e24dcca9e";
pub const COMMAND_UUID: &str = "0000ff11-0000-1000-8000-00805f9b34fb";
pub const DATA_UUID: &str = "0000ff12-0000-1000-8000-00805f9b34fb";
/// 订阅 notify 后到首笔写之间的最小间隔（协议规则 §2，iOS 系坑的双侧容忍）。
pub const NOTIFY_SETTLE: Duration = Duration::from_millis(1600);

pub struct DiscoveredDevice {
    pub id: PeripheralId,
    pub name: String,
    pub rssi: i16,
}

/// 扫描 `duration`，返回名称匹配 `name_prefix`（前缀可为空=全部）的设备。
pub async fn scan(
    adapter: &Adapter,
    name_prefix: &str,
    duration: Duration,
) -> Result<Vec<DiscoveredDevice>> {
    adapter
        .start_scan(ScanFilter::default())
        .await
        .map_err(|e| SdkError::Ble(e.to_string()))?;
    tokio::time::sleep(duration).await;
    adapter.stop_scan().await.ok();

    let mut out = Vec::new();
    for p in adapter
        .peripherals()
        .await
        .map_err(|e| SdkError::Ble(e.to_string()))?
    {
        // 见下方过滤：全部候选先记 debug 便于排障
        let Ok(Some(props)) = p.properties().await else {
            continue;
        };
        let name = props.local_name.unwrap_or_default();
        // rssi 缺省（i16::MIN）多为 macOS 本地缓存中的已知设备，不一定是真实广播
        tracing::debug!(target: "dyj.ble", name = %name, rssi = ?props.rssi, "scan candidate");
        if !name_prefix.is_empty() && !name.starts_with(name_prefix) {
            continue;
        }
        tracing::info!(target: "dyj.ble", name = %name, rssi = ?props.rssi, "device discovered");
        out.push(DiscoveredDevice {
            id: p.id(),
            name,
            rssi: props.rssi.unwrap_or(i16::MIN),
        });
    }
    out.sort_by_key(|d| std::cmp::Reverse(d.rssi));
    Ok(out)
}

/// 已连接设备的传输句柄：notify 帧文本经 [tx] 上行；[write_line] 下行。
pub struct ConnectedTransport {
    pub peripheral: Peripheral,
}

/// 连接 + 服务发现 + 订阅 FF11/FF12，启动 reader 任务：
/// FF11 notify → 帧文本发到 `text_tx`；FF12 notify → 二进制帧发到 `binary_tx`。
pub async fn connect(
    adapter: &Adapter,
    id: &PeripheralId,
    text_tx: mpsc::Sender<String>,
    binary_tx: mpsc::Sender<crate::protocol::BinaryFrame>,
) -> Result<ConnectedTransport> {
    let peripheral = adapter
        .peripheral(id)
        .await
        .map_err(|e| SdkError::Ble(e.to_string()))?;
    tracing::info!(target: "dyj.ble", "connecting...");
    peripheral
        .connect()
        .await
        .map_err(|e| SdkError::Ble(format!("connect: {e}")))?;
    tracing::info!(target: "dyj.ble", "connected; discovering services");
    peripheral
        .discover_services()
        .await
        .map_err(|e| SdkError::Ble(format!("discover: {e}")))?;

    let find_char = |uuid: &str| {
        peripheral
            .characteristics()
            .into_iter()
            .find(|c| c.uuid.to_string().to_lowercase() == uuid)
    };

    let command_char = find_char(COMMAND_UUID).ok_or(SdkError::InvalidFrame(
        "FF11 command characteristic not found (is this a DYJ device?)".into(),
    ))?;

    peripheral
        .subscribe(&command_char)
        .await
        .map_err(|e| SdkError::Ble(format!("subscribe: {e}")))?;
    tracing::info!(target: "dyj.ble", "FF11 subscribed");

    // FF12 可选订阅（旧固件可能没有）：缺失不阻断连接
    let data_char = find_char(DATA_UUID);
    if let Some(ref data) = data_char {
        peripheral
            .subscribe(data)
            .await
            .map_err(|e| SdkError::Ble(format!("subscribe FF12: {e}")))?;
        tracing::info!(target: "dyj.ble", "FF12 subscribed");
    } else {
        tracing::warn!(target: "dyj.ble", "FF12 not found; realtime audio unavailable");
    }
    tokio::time::sleep(NOTIFY_SETTLE).await;

    // reader 任务：notify 按 uuid 分流（clone tx 进任务，原句柄留在结构体）
    let mut notifications = peripheral
        .notifications()
        .await
        .map_err(|e| SdkError::Ble(e.to_string()))?;
    let text_task = text_tx.clone();
    let binary_task = binary_tx.clone();
    tokio::spawn(async move {
        let mut assembler = crate::protocol::FrameAssembler::default();
        let mut binary_asm = crate::protocol::BinaryFrameAssembler::default();
        while let Some(note) = notifications.next().await {
            let uuid = note.uuid.to_string().to_lowercase();
            if uuid == DATA_UUID {
                for frame in binary_asm.feed(&note.value) {
                    if binary_task.send(frame).await.is_err() {
                        break;
                    }
                }
            } else {
                let text = String::from_utf8_lossy(&note.value).to_string();
                for line in assembler.feed(&text) {
                    if text_task.send(line).await.is_err() {
                        break;
                    }
                }
            }
        }
        // stream 结束 = 断连；tx drop 由持有方感知
    });

    Ok(ConnectedTransport { peripheral })
}

impl ConnectedTransport {
    /// 发送一帧文本（整帧含定界符）。MTU 517 已真机验证，命令帧远小于该值，无需分片。
    pub async fn write_line(&self, line: &str) -> Result<()> {
        tracing::debug!(target: "dyj.ble", line = %line, "write");
        let frame = format!("{line}{FRAME_SUFFIX}");
        self.peripheral
            .write(
                &self.command_characteristic()?,
                frame.as_bytes(),
                WriteType::WithResponse,
            )
            .await
            .map_err(|e| SdkError::Ble(format!("write: {e}")))
    }

    /// 显式断开（macOS CoreBluetooth：drop Peripheral 不会断开连接，必须显式调用）
    pub async fn disconnect(&self) -> Result<()> {
        tracing::info!(target: "dyj.ble", "disconnecting");
        self.peripheral
            .disconnect()
            .await
            .map_err(|e| SdkError::Ble(e.to_string()))
    }

    pub async fn is_connected(&self) -> Result<bool> {
        self.peripheral
            .is_connected()
            .await
            .map_err(|e| SdkError::Ble(e.to_string()))
    }

    fn command_characteristic(&self) -> Result<btleplug::api::Characteristic> {
        self.peripheral
            .characteristics()
            .into_iter()
            .find(|c| c.uuid.to_string().to_lowercase() == COMMAND_UUID)
            .ok_or_else(|| SdkError::InvalidFrame("FF11 lost".into()))
    }
}

const FRAME_SUFFIX: &str = "\r\n";
