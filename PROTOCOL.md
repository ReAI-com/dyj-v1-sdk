# 点一机 1 代协议规则（设计层）

> 状态：规则已定稿（proto_version=1）；§9 命令/事件登记表已随 v1 首版实施填充完毕，所列命令均已在设备固件实施并真机验证（详见 [CHANGELOG.md](CHANGELOG.md) 各版本配套固件）。
> 读者：设备固件实现者、本 SDK 实现者、近场客户端（小程序等）开发者。
> 本文档是协议的**单一事实源**：任何协议变更必须先改本文档，再改实现（固件与 SDK 同步遵守）。
> 本文档随 reai-dyj-sdk 仓库发布；设备固件与协议演进的上游在内部主仓库，两处保持同步。

## 1. 定位

设备（GATT Server 外围）与近场客户端（SDK/小程序/调试工具）之间的通信协议；同一套命令模型也映射到云端通道（WS 控制 + HTTP 数据面）。协议设计目标：

- **稳定优先**：单命令语义简单、可预测，失败可恢复
- **前向兼容**：新命令/新事件不影响旧客户端，未知内容安全忽略
- **双通道一致**：BLE 与云端行为一致，客户端代码不因通道而分叉
- **继承血泪**：直接吸收 v2 与 reai-card-sdk 的事故教训（见 §10）

## 2. GATT 布局

沿用 v2 布局（近场生态兼容，现有小程序可直接对接；UUID 以 v2 实测为准）：

| 项 | UUID | 属性 | 说明 |
|---|---|---|---|
| 服务 | `6e400001-b5a3-f393-e0a9-e50e24dcca9e` | — | Nordic UART Service（NUS） |
| 命令特征 FF11 | `0000FF11-0000-1000-8000-00805F9B34FB` | Read, Write, WriteNoRsp, Notify | 文本帧：命令/响应/事件（双向） |
| 数据特征 FF12 | `0000FF12-0000-1000-8000-00805F9B34FB` | Read, Write, WriteNoRsp, Notify | 二进制帧（**双向**：客户端→设备写、设备→客户端 notify） |
| 通知描述符 | `00002902-...` | — | 标准 CCCD |

规则：

- 广播名以 `DYJ_` 前缀 + 设备标识后缀（v2 模式），广播包内不含敏感信息
- FF12 双向复用：客户端→设备方向的二进制（如上行音频）走 write；设备→客户端方向走 notify，帧头 type 区分方向语义
- 客户端写特征一律**串行队列**（等待上一写完成/收到响应再写下一笔），禁止并发写
- 订阅 Notify 后延时 ≥1500ms 再发首条写命令（iOS 侧已知坑，双侧都要容忍）

## 3. 帧格式

### 3.1 文本帧（命令 / 响应 / 语义事件）

```
CMD:{"id":1,"cmd":"wifi.list","params":{...}}\r\n     ← 客户端 → 设备（请求）
RSP:{"id":1,"code":0,"msg":"ok","data":{...}}\r\n     ← 设备 → 客户端（响应）
EVT:{"evt":"button.click","ts":1698...,"data":{...}}\r\n ← 设备 → 客户端（主动事件）
```

- v2 兼容：v2 的 `CMD:JSON\r\n` 是本格式的无 `id` 子集；设备端必须同时接受带 `id` 与不带 `id` 的请求
- 编码 UTF-8，单帧 JSON 必须完整（不做文本帧分片；超长响应拆分见 §3.3）
- `\r\n` 为帧定界符，JSON 内不得出现裸换行

### 3.2 二进制帧（数据特征，新增设计）

```
偏移  0      1        2..3     4..5     6...
     +------+------+--------+--------+--------------+
     | type | flags| seq_le | len_le | payload      |
     +------+------+--------+--------+--------------+
     type:   1 字节，载荷类型（见下表）
     flags:  1 字节，bit0=分片起始，bit1=分片结束，bit2=需要 ACK（其余保留）
     seq_le: 2 字节小端，流内递增序号（回绕），用于丢包检测
     len_le: 2 字节小端，payload 长度（不含 6 字节头）
```

| type | 含义 | 说明 |
|---|---|---|
| `0x00` | 旧版音频（v2 兼容） | v2 的裸 `0x00 前缀 + ≤200B AMR-WB 包`，保留只读支持 |
| `0x01` | 实时音频（ADPCM，预留） | IMA ADPCM 4:1。**当前设备未启用**：真机实测 BLE 链路 ~3.6KB/s 放不下 16kHz ADPCM（8KB/s）；编码器与 SDK 解码器保留，供快链路/卡片对齐复用 |
| `0x02` | 文件数据块 | 与 `file.*` 命令配合，分片传输 |
| `0x03` | 心跳 | 空载荷，保活与丢包探测 |
| `0x04` | 实时音频（AMR-WB，**现行**） | AMR-WB 16kHz 宽带 12.65kbps（v2 同款）。payload = N 个标准 AMR 文件格式帧（ToC 带 Q 位 + 载荷），20ms/帧，5 帧批 ≈100ms/包（165B，兼容 iOS MTU 185） |
| `0x05`+ | 保留 | 新增数据类型在此登记，未知 type 收到即丢弃并计数 |

- **类型头的意义**：实时音频与文件块可在同一特征上并存，不再互斥（规避 reai-card-sdk 的互斥教训）
- 分片规则：按 `MTU-3` 切分（MTU 未协商成功时按 20B 最坏情况）；重组按 seq 连续性校验，断流即丢弃整个传输单元
- 需 ACK 的传输（文件块）由接收方周期回 `RSP` 进度或最终结果；音频流不 ACK，靠 seq 丢包信号让上层重置解码状态

### 3.3 列表/大响应分片

继承 v2 语义：响应的 `data` 可带 `index`（从 0 递增）与 `more`（false 结束）字段分多条 `RSP` 发送；客户端按 `id` 聚合。

## 4. 命令模型

- 命令名：小写点分域，`<域>.<动作>`（如 `wifi.connect`、`led.status`、`file.list`）；域即能力域（control/led/camera/button/sensor/system/wifi/audio/file，upload/ota 随实施新增登记于 §9）
- 请求 `{id, cmd, params}`；响应 `{id, code, msg, data}`；`id` 由客户端生成，设备原样返回
- **并发规则**：协议无服务端并发能力，命令**严格串行**执行（单协议 worker，先到先执行）；`id` 用于响应匹配与超时对账，不用于并行加速（reai-card-sdk 同款约束）。文件传输等长命令期间，其他命令排队等待
- 每条命令有**超时**（§7），客户端超时未收到 `RSP` 即失败，可重试
- 云端映射：WS 通道收发同一 JSON（去 `\r\n` 定界符，按 WS 消息边界分帧）；HTTP 数据面（上传/下载）不走命令模型，走资源接口

## 5. 鉴权分级

| 级别 | 名称 | 要求 | 典型命令 |
|---|---|---|---|
| L0 | 公开 | 任何已连接客户端 | 协议版本/能力查询、设备基础信息 |
| L1 | 普通 | 已连接（默认） | LED 查询、传感器读取、音量、WiFi 扫描 |
| L2 | 授权 | 已授权连接（配对或 token） | 拍照、文件 take/peek、WiFi 连接/改配置、绑定表修改 |
| L3 | 管理 | 授权 + 显式确认（命令内 `confirm:true`） | 重启/关机/OTA/服务端地址修改/USB 调试 |

规则：

- **所有命令都有级别**，默认 L1；未授权客户端调用 L2+ 返回统一错误码（§6）
- 云端通道鉴权走 token（继承 v2 AuthManager 体系，补防爆破与刷新逻辑）
- BLE 侧授权的具体机制（配对码/token 下发命令）属 §9 待实施项，但**分级框架与错误码现在定死**
- 破坏性/系统命令（L3）SDK 侧同样要求显式 opt-in API，双侧对齐

## 6. 错误码

数字分段，`msg` 必须人可读且**给出修复动作**（参考 reai-card-sdk 的语义化错误）：

| 段 | 含义 |
|---|---|
| 0 | 成功 |
| 1xxx | 通用错误（1001 未知命令、1002 参数错误、1003 内部错误、1004 忙/重试） |
| 2xxx | 鉴权（2001 未授权、2002 token 失效、2003 需确认、2004 级别不足） |
| 3xxx | 状态冲突（3001 采集设备占用、3002 正在 OTA、3003 未配网…） |
| 4xxx | 硬件（4xxx-相机/4xxx-传感器待实施时登记） |
| 9xxx | 保留（协议不兼容等） |

LED 域细分（4000-4099 预留，当前使用 4401+）：

| 码 | 含义 | msg 要求（含修复动作） |
|---|---|---|
| 4401 | 未知 pattern | 列出 supported_patterns（见 led.status） |
| 4402 | priority 非法（非整数数值） | 说明合法范围 100-900、默认 850（越界数值会被 clamp，不报错） |
| 4403 | brightness 非法 | 列出合法值 lower/low/mid/high |

未在表内的码按 `1xxx` 通用错误处理；错误码登记处 = 本文档 §9 命令表旁注。

button 域不新开错误码段：参数错误复用通用码 1002（2026-10-26 登记）。

file/upload/ota 域不新开错误码段：复用 1002 参数 / 1003 内部与网络失败 / 3001 存储不足与文件占用 / 3003 未配网（WiFi 未连接）/ 2003 需确认（2026-10-27 登记）。

## 7. 超时表

- 默认命令超时 **10s**；每条命令在 §9 表中登记专用超时的必须写理由
- 预期长命令：WiFi 扫描/连接（35s）、OTA 检查/下载（分段回报进度）、文件传输（按大小协商，带进度）
- 设备端执行超时自行中断并返回错误码，不允许无界挂起

## 8. 连接生命周期与能力协商

```
广播 → 连接 → 服务发现/订阅 → HELLO 握手 → (L0 命令可用) → 授权 → (L1/L2 可用) → 确认 → (L3 可用)
```

- **HELLO 握手**（新命令，首批实施）：客户端与设备交换 `{proto_version, feature_bits, name, fw/app 版本}`
- **能力协商用 feature bits 而非版本号比较**（open-sdk 原则）：双方各自声明支持位，交集生效
- 断连：设备侧释放该连接的资源（会话、待确认状态），广播恢复；**GATT Server close-before-open 铁律**（app 实现责任）
- 设备同一时间允许多少 BLE 主连接：1 个（v2 现状），多连接留待后续版本用 feature bits 协商

## 9. 命令与事件登记表（实施时填充）

> 规则：新命令/事件实施前必须在此登记——命令名、级别、超时、params/data 结构、错误码、所属能力域、实现状态。**文档先于代码。**

### 9.1 命令（待实施填充）

| cmd | 级别 | 超时 | 说明 | 状态 |
|---|---|---|---|---|
| `proto.hello` | L0 | 5s | 握手：版本+feature bits 交换。data: `{proto_version, feature_bits, device, app_version, client_feature_bits}` | ✅ 已实施并真机验证（2026-10-24） |
| `proto.echo` | L0 | 10s | 回声联调。params: `{message}` → data: `{message}` | ✅ 已实施并真机验证（2026-10-24） |
| `device.info` | L1 | 5s | 设备身份与系统信息。默认核心字段 `{device_id, device_name, model, android_version, app_version}`；params `{verbose:true}` 附开发字段（imei/board/fingerprint/bt_address/uptime 等）；不可用字段为 null | ✅ 已实施 |
| `device.battery` | L1 | 5s | 电量与充电状态。data: `{level, status, plugged, voltage_mv}`（status: charging/full/discharging） | ✅ 已实施 |
| `device.storage` | L1 | 5s | 存储余量。data: `{data_total_bytes, data_free_bytes, data_free_mb}`（/data 分区） | ✅ 已实施 |
| `audio.stream.start` | L1* | 5s | 开启实时音频流。data: `{sample_rate:16000, channels:1, encoding:"amr-wb", bitrate_bps:12650, frame_ms:20, frames_per_block:5, block_bytes:165}`。流走 FF12 type=0x04；断连自动停。*规划目标 L2，授权机制实施后收紧 | ✅ 已实施（2026-10-24） |
| `audio.stream.stop` | L1* | 5s | 关闭实时音频流。data: `{blocks_sent}` | ✅ 已实施（2026-10-24） |
| `audio.capture.start` | L1* | 5s | 开启采集引擎（mic+AMR-WB 编码，stream/record 的数据源）。data: `{sample_rate, channels, encoding, bitrate_bps, engine_running}`。幂等 | ✅ 已实施（2026-10-26） |
| `audio.capture.stop` | L1* | 5s | 关闭采集引擎；stream/record 开关状态不动（引擎恢复后数据自动续上）。data: `{engine_running, blocks_sent, captured_ms}` | ✅ 已实施（2026-10-26） |
| `record.start` | L1* | 5s | 开始本地录音落盘（引擎未开自动开启）。params: `{segment_min?: 10, session?: 8位大写hex}`（session 缺省设备生成，非法直接拒绝 1002——对齐卡片 0.3.8）；data: `{dir:"files/audio", format:"amr-wb", segment_min, session, seq:1}`。**会话命名规则**：`audio_YYYYMMDD-HHMMSS_<session>_<seq>.amr`（如 `audio_20261026-153000_A1B2C3D4_01.amr`），同一次录音（start→stop）所有分段共享 session、seq 从 1 递增，服务端/SDK 按名分组；≈1MB/10min；剩余空间 <100MB 拒绝（3001） | ✅ 已实施（2026-10-26） |
| `record.stop` | L1* | 5s | 停止本地录音。data: `{last_file, session, files_written, bytes_written, engine_running}`（session=刚结束会话编号，对齐卡片 stopped 推送语义；之后新 start 生成新 session） | ✅ 已实施（2026-10-26） |
| `record.status` | L1* | 5s | 录音状态。data: `{recording, engine_running, files, bytes_total, current_file, last_file, session?, seq?}`（session/seq 仅录音中携带） | ✅ 已实施（2026-10-26） |
| `record.list` | L1* | 10s | 录音文件列表。**indexed 流式响应（§3.3）**：一条 entry 一帧 `{index, more:true, entry:{name, bytes, mtime, session?, seq?}}`（session/seq 从文件名解析，旧命名文件不带），收尾帧 `{index:N, more:false, total:N}`；按文件名（时间）升序。SDK `record_list()` 自动聚合为 `{total, items}` | ✅ 已实施（2026-10-26） |
| `led.status` | L1 | 5s | 查询当前灯效与 claim 栈。**indexed 流式响应（§3.3，record.list 同款）**：entry 0 = `{active:{owner, pattern, brightness, source:"system"\|"override", remaining_ms?}或null}`，entry 1..N = 每条 claim 一帧 `{owner, priority, pattern, brightness, source, remaining_ms?}`（priority 降序）。SDK `led_status()` 聚合重塑为 `{active, claims, total}`。**为何分帧**：整包在 3+ claim 时 >600B 超 MTU-3，被栈静默截断（客户端收到残缺 JSON 丢弃→连环超时，2026-10-26 真机事故）。supported_patterns/brightness 不上线路（静态规范见本行） | ✅ 已实施（2026-10-26） |
| `led.set` | L1* | 5s | 临时设置自定义灯效（不持久化，重启即清）。params: `{pattern, brightness?:"lower"\|"low"\|"mid"\|"high"(默认low), priority?:(100-900，默认850，越界 clamp), timeout_s?:(1-3600，默认不带=一直生效直到 clear/重启)}` → data: `{accepted:true, owner:"ble-override"}`。同一 owner 重复 set 覆盖旧 claim；被更高优先级系统状态（如录音 600）压制时 accepted 仍 true，灯效在系统状态解除后自动生效。错误码：4401/4402/4403（§6）。*规划目标 L2，授权机制实施后收紧 | ✅ 已实施（2026-10-26） |
| `led.clear` | L1* | 5s | 清除 `ble-override` 与 `fault` claim 并回落到剩余最高优先级 claim。data: `{cleared:["ble-override"...实际清除的 owner], active:{...}或null}`。*规划目标 L2，授权机制实施后收紧 | ✅ 已实施（2026-10-26） |
| `wifi.scan` | L1 | 35s | 扫描周围 WiFi。**indexed 流式响应（§3.3，MTU 185 铁律——led.status 同款教训：10 网络 ≈900B 单帧必被截断）**：一条 network 一帧 `{index, more:true, entry:{ssid,bssid,rssi,security:"open"\|"wpa2"\|"wpa3"\|"unknown",frequency}}`，收尾帧 `{more:false,total}`；SDK `wifi_scan()` 聚合 `{total, items}`。按 rssi 降序、同名 SSID 去重取最强。系统定位开关关闭时拒绝（错误 3401，不返回空列表装成功）。35s：含 startScan 等待（~8s） | ✅ 已实施并真机验证（2026-10-26） |
| `wifi.connect` | L2* | 35s | 连接 WiFi。params: `{ssid, password?, bssid?}`（password 缺省=开放网络）→ data: `{connected:bool, ssid, ip?, phase?}`。同步等待：30s 轮询 SupplicantState COMPLETED + DHCP 到手（≤8s）；失败 `connected:false` 带阶段 `phase:"auth"\|"dhcp"\|"timeout"`；失败自动清理新配置并恢复原网络。*规划目标 L2，授权机制实施后收紧 | ✅ 已实施并真机验证（2026-10-26） |
| `wifi.status` | L1 | 5s | WiFi 状态。data: `{wifi_enabled, state, ssid?, bssid?, ip?, rssi?, link_speed_mbps?, frequency?, gateway?}`（不可用字段 null，对齐 device.* 约定） | ✅ 已实施并真机验证（2026-10-26） |
| `wifi.forget` | L2* | 5s | 删除已保存网络。params: `{ssid?}`（缺省=删除全部）→ data: `{removed:n}`；删除当前连接的网络时同步断开并触发系统重选。*规划目标 L2，授权机制实施后收紧 | ✅ 已实施并真机验证（2026-10-26） |
| `ap.start` | L2* | 35s | 开热点（SoftAP）。params: `{ssid?, password?, allow_sta_drop?:bool=false}`（缺省 SSID=`DYJ_<device_id后4位>`、密码 `reai_dyj_2026`；参数不持久化，每次显式给）→ data: `{active:true, ssid, password, ip, sta_dropped:bool}`。**并发策略**：直接开 AP 不动 STA → 起来后查 STA 存活：存活=`sta_dropped:false`；掉了且未 allow_sta_drop → 自动回滚（关 AP + 恢复 STA）+ 错误 3502 sta_concurrency_unsupported。AP 激活期间 8788 端口提供 HTTP 文件服务（录音快传与 OTA 包上传，端点见 §11）。*规划目标 L2，授权机制实施后收紧 | ✅ 已实施并真机验证（2026-10-26）。**SPRD 实测定论**：不支持 AP+STA 并发（AP 起在 wlan0，STA 让路）；未放行时自动回滚 + 3502 真机验证。注意：8.1 的 WifiManager.setWifiApEnabled 是官方桩（恒 false），实际走 IWifiManager binder 直调（oneway/void，成败以状态轮询为准） |
| `ap.stop` | L2* | 5s | 关热点。data: `{active:false}`；已保存 STA 网络由系统自动恢复。8788 文件服务随停。*规划目标 L2，授权机制实施后收紧 | ✅ 已实施并真机验证（2026-10-26） |
| `ap.status` | L1 | 5s | 热点状态。data: `{active, ssid?, ip?, sta_state:"connected"\|"disconnected"\|"unknown"}`（ip 以实际接口枚举为准，Android 惯例 192.168.43.x 网段） | ✅ 已实施并真机验证（2026-10-26） |
| `button.status` | L1 | 5s | 按键能力查询。data: `{supported_gestures:["click","double_click","long_press"], thresholds:{debounce_ms:50,double_click_ms:300,long_press_ms:500}, bindings:{click,double_click,long_press}, last_event?}`（thresholds 只读展示，第一版不可配；last_event 为最近一条 button.event 的 `{type,duration_ms?}`，设备启动后无事件则不带） | ✅ 已实施并真机验证（2026-10-26） |
| `button.bind.get` | L1 | 5s | 查询手势→动作绑定表。data: `{bindings:{click:"event_only",double_click:"event_only",long_press:"event_only"}}` | ✅ 已实施并真机验证（2026-10-26） |
| `button.bind.set` | L2* | 5s | 设置单个手势→动作绑定（单对设置，不做批量）。params: `{gesture:"click"\|"double_click"\|"long_press", action:"event_only"\|"record.toggle"\|"stream.toggle"\|"capture.toggle"}` → data: `{bindings:{...}}`。gesture/action 非法拒绝 1002（msg 列出合法值）；立即生效 + 持久化（重启保留）；绑定动作执行失败只记日志，不影响事件发布与绑定本身。预留动作（随对应域落地登记，当前 set 拒绝）：`wifi.ap.toggle`、`ota.confirm`。*规划目标 L2，授权机制实施后收紧 | ✅ 已实施并真机验证（2026-10-26） |
| `record.delete` | L2* | 5s | 删除录音分段（files/audio，白名单 = 新旧命名 `audio_*.amr`）。params: `{name}` → data: `{deleted:true, freed_bytes}`。名字不符白名单或不存在 1002（msg 提示用 record.list 查名）；删正在录音的当前分段 3001。已上传文件可直接删（索引随删剪除）。*规划目标 L2，授权机制实施后收紧 | ✅ 已实施并真机验证（2026-10-27） |
| `upload.config.get` | L1 | 5s | 查询上传配置。data: `{url(生效值), overridden(是否被 set 覆盖), default_url, token_set}`。三级读取：DataStore 覆盖 → 资源默认 → 硬编码兜底 | ✅ 已实施并真机验证（2026-10-27） |
| `upload.config.set` | L2* | 5s | 设置上传 endpoint。params: `{url?, token?}`（url 空串=清覆盖回落默认；token 可选，作 `authorization: Bearer` 头，生产鉴权预留位）→ data: `{url, overridden}`。持久化重启保留。*规划目标 L2，授权机制实施后收紧 | ✅ 已实施并真机验证（2026-10-27） |
| `upload.start` | L2* | 5s | 触发录音上传（手动；自动上传不启用，卡片同款）。params: `{name?}`（缺省=全部未上传，按已传索引过滤）→ data: `{queued:n, skipped:n}` **快速受理**，上传后台执行（与协议 worker 解耦）。WiFi 未连接拒绝 3003；名字非法 1002；已在上传中则合并队列。5s 超时理由：只受理不等传输 | ✅ 已实施并真机验证（2026-10-27） |
| `upload.status` | L1 | 5s | 上传状态。data: `{uploading, current_file?, pending(未上传数), uploaded(已上传数), last:{name, state, bytes, error?}}` | ✅ 已实施并真机验证（2026-10-27） |
| `ota.check` | L1 | 10s | 检查 OTA 清单（GET 可配置清单 URL；静态 JSON `{version, versionName?, versionCode, url, sha256?, size?, force?}`，缺 versionCode/url = 非法清单 1003）。data: `{need_update, current:{version_name, version_code}, remote{...}}`。need_update = versionCode 比较（>当前；== 且 force 才算）。WiFi 未连 3003；网络失败 1003 带 msg。10s 超时理由：外网 HTTPS | ✅ 已实施并真机验证（2026-10-27） |
| `ota.install` | L2*→L3 | 5s | 触发 OTA（check→download→verify→pm install 状态机后台跑，进度见 ota.progress）。**源二选一：URL 清单（默认）或 AP 通道暂存包**（`POST /api/ota/upload` 暂存的 staged.apk 优先，见 §11）。params: `{confirm:true}` **必填**（缺 2003 需确认）。安装前置校验：清单 size/sha256 有则必比 + APK 包名必须为本应用 + APK 签名证书==当前 app（防投毒）+ versionCode 单调（<当前拒绝；== 需清单 force）；存储前置 StatFs≥size+50MB（3001）；**安装失败绝不重启进程/设备**。data: `{accepted:true}`。5s 超时理由：只受理；*规划目标 L3（授权+确认），授权机制实施后收紧 | ✅ 已实施并真机验证（2026-10-27） |
| `ota.status` | L1 | 5s | OTA 状态。data: `{state:"idle"\|"checking"\|"downloading"\|"verifying"\|"installing"\|"failed", progress?, size?, error?}` | ✅ 已实施并真机验证（2026-10-27） |
| `ota.config.get` / `ota.config.set` | L1 / L2* | 5s | OTA 清单 URL 查询/覆盖（三级读取，默认为部署侧静态清单 URL；改址命令保留 = 防变砖约束：地址绝不编译期写死）。set params `{url?}` 空串清覆盖 → data 同 upload.config | ✅ 已实施并真机验证（2026-10-27） |
| `camera.status` | L1 | 5s | 摄像头状态。data: `{available, recording, photo_count, video_count}`（camera 域不新开错误码段：复用 1002 参数 / 1003 内部 / 3001 域内资源互斥与存储不足——capture/record 互相占用、删录像中文件；3002 已被 OTA 占用勿混用） | ✅ 已实施并真机验证（2026-10-29，拍照/上传/删除/录像） |
| `camera.capture` | L2* | 10s | 拍照落盘 `files/camera/img_YYYYMMDD-HHMMSS-SSS.jpg`（毫秒防同秒碰撞）→ `{name, bytes}`。EXIF orientation 摆正。媒体经 `upload.start` 走 WiFi 上传（BLE 链路 ~KB/s 不传媒体）；`*规划目标 L2，授权机制实施后收紧` | ✅ 已实施（2026-10-29） |
| `camera.record.start` | L2* | 5s | 开始录像。params: `{max_s?:600(上限 1800)}`（到时自动停）→ `{recording:true, name}`（设备公共存储 `DYJ/video_YYYYMMDD-HHMMSS-SSS.mp4`——SELinux mediaserver 策略不允许写应用私有目录）。**纯视频不含音频**；码率钳 4Mbps；存储前置 StatFs≥50MB（3001）；capture/record 域内互斥，占用中 3001 | ✅ 已实施并真机验证（2026-10-29，录像/删除全链路） |
| `camera.record.stop` | L2* | 5s | 停止录像 → `{name, bytes, duration_s}` | ✅ 已实施（2026-10-29） |
| `camera.record.status` | L1 | 5s | 录像状态。data: `{recording, name?, elapsed_s, max_s}` | ✅ 已实施（2026-10-29） |
| `camera.list` | L1 | 10s | 相机文件列表（files/camera）。**indexed 流式响应（§3.3，record.list 同款）**：一条 entry 一帧 `{index, more:true, entry:{name, bytes, mtime, kind:"photo"\|"video"}}`，收尾帧 `{more:false,total}`；按名内时间戳升序（跨类型字典序 ≠ 时间序）。SDK `camera_list()` 自动聚合 `{total, items}` | ✅ 已实施（2026-10-29） |
| `camera.delete` | L2* | 5s | 删除相机文件。params: `{name}` → `{deleted:true, freed_bytes}`；白名单外/不存在 1002；删录像中当前文件 3001 | ✅ 已实施（2026-10-29） |
| `sensor.status` | L1 | 5s | 传感器能力与流状态。data: `{available:["magnetic","orientation","pressure","step_counter","rotation_vector"], streaming, types, interval_ms?}`（加速度计为桩、光感/距离感无 HAL → 不在列，请求即 1002；sensor 域不新开错误码段：复用 1002/1003，无即时采样帧是正常业务结果不落错误码） | ✅ 已实施并真机验证（2026-10-30，批 2） |
| `sensor.read` | L1 | 5s | 单次实读（采帧即注销）。params: `{types?:[...]}`（子集，缺省=全部可用）→ data: `{values:{pressure_hpa, magnetic_ut:{x,y,z}, orientation_deg:{azimuth,pitch,roll}, steps, rotation:{x,y,z,w}}}`（字段可选，仅已交付类型携带；steps 为开机以来累计值；桩/未知类型 1002 msg 列 available） | ✅ 已实施并真机验证（2026-10-30，批 2） |
| `sensor.stream.start` | L2* | 5s | 开启限流订阅。params: `{types?, interval_ms?:1000(下限 500)}` → `{streaming:true, interval_ms}`；EVT `sensor.data` 周期发、一帧合并多类型最新值；重复 start = 重置；断连自动停。`*规划目标 L2，授权机制实施后收紧` | ✅ 已实施并真机验证（2026-10-30，批 2） |
| `sensor.stream.stop` | L2* | 5s | 停止订阅。data: `{streaming:false, frames_sent}` | ✅ 已实施并真机验证（2026-10-30，批 2） |
| `location.status` | L1 | 5s | 定位状态。data: `{enabled, gps_active, streaming, has_fix, lat?, lon?, accuracy_m?, altitude_m?, speed_mps?, bearing_deg?, satellites?, last_fix_age_s?}`（enabled=系统定位模式非 OFF；has_fix 基于 lastKnownLocation 带 last_fix_age_s；不可用字段省略） | ✅ 已实施并真机验证（2026-10-30，批 2） |
| `location.get` | L2* | 30s | 取一次定位。params: `{timeout_s?:20(1..25)}` → 有 fix 返回 fix 字段组（has_fix:true），**无 fix 正常返回 `{has_fix:false}` 不报错**（室内无星常态，超时是业务结果）。前置自动开定位模式（system uid 直写 Secure 设置；wifi.scan 的 3401 行为不受影响）；30s 超时理由：GNSS 冷启动 TTFF 不可预期，25s 参数上限 + 5s 余量。`*规划目标 L3（隐私敏感），授权机制实施后收紧` | ✅ 已实施并真机验证（2026-10-30，批 2） |
| `location.stream.start` | L2* | 5s | 开启定位订阅。params: `{min_interval_s?:10(下限 5)}` → `{streaming:true, min_interval_s}`；EVT `location.fix` 仅新 fix 才发；重复 start = 重置；断连自动停。`*规划目标 L3（隐私敏感）` | ✅ 已实施并真机验证（2026-10-30，批 2） |
| `location.stream.stop` | L2* | 5s | 停止订阅。data: `{streaming:false, fixes_sent}` | ✅ 已实施并真机验证（2026-10-30，批 2） |
| `vibrator.once` | L2* | 5s | 震动一次。params: `{duration_ms?:200(1..10000), amplitude?:默认强度(1..255)}` → `{accepted:true}`；越界 1002；波形 pattern 留待定项；sysctrl 域不新开错误码段（复用 1002/1003/2003） | 🚧 已登记（2026-10-30，批 3） |
| `vibrator.cancel` | L2* | 5s | 取消当前震动。data: `{cancelled:true}` | 🚧 已登记（2026-10-30，批 3） |
| `volume.get` | L1 | 5s | 三流音量。data: `{streams:{music:{current,max}, ring:{current,max}, system:{current,max}}}` | 🚧 已登记（2026-10-30，批 3） |
| `volume.set` | L2* | 5s | params: `{stream:"music"\|"ring"\|"system", value:int}` → `{stream, current}`（set 后回读生效值，越界 clamp；stream 非法 1002） | 🚧 已登记（2026-10-30，批 3） |
| `device.time` | L2* | 5s | 无 params 回读 `{epoch_ms, uptime_ms}`；`{epoch_ms}` 对时 → `{epoch_ms, applied:true}`（AlarmManager.setTime，网络自动对时开启会被系统覆盖） | 🚧 已登记（2026-10-30，批 3） |
| `device.reboot` | L3 | 5s | **全协议最危险命令**：`{confirm:true}` 必填（缺 2003）→ `{accepted:true}` 后延迟 1s 重启（响应先出 BLE）；BLE 断连属预期；REBOOT 签名权限仅平台签名 sys 包持有 | 🚧 已登记（2026-10-30，批 3） |
| *(其余随实施登记)* | | | | |

### 9.2 事件（待实施填充）

| evt | 说明 | 状态 |
|---|---|---|
| `record.segment` | 录音分段滚动：`{file, bytes, session, seq, duration_s?}`（分段关闭时发，session/seq 标记所属会话——后续上传调度与云端按名分组以此为准） | ✅ 已实施（2026-10-26） |
| `led.state` | 生效灯效变化：`{owner, pattern, brightness, source:"system"\|"override"}`（含 claim 新增/解除/超时回落导致的任何生效灯效切换；brightness 对 off 为 null） | ✅ 已实施（2026-10-26） |
| `conn.wifi` | WiFi 连接状态变化：`{state:"connected"\|"disconnected", ssid?, ip?}`（CONNECTIVITY_ACTION 派生 + 3s 去抖，仅状态实际变化时发；喂 LED 与后续云端通道。同一馈线维持 wifi LED 档） | ✅ 已实施并真机验证（2026-10-26） |
| `button.event` | 按键事件（原始 + 手势全量，恒发）：`{type:"down"\|"up"\|"click"\|"double_click"\|"long_press_start"\|"long_press_end", duration_ms?}`。down/up 为原始键事件（up 带已按时长，down 无 duration）；手势带该次按压时长（long_press_end 为全按压时长，double_click 为第二按压时长，时长均由输入子系统 uptime eventTime 计算、不受转发延迟影响）。绑定动作在 long_press_start/click/double_click 触发（toggle 语义：下次同手势再触发翻转）；事件发布与动作执行解耦。帧级 ts 见 EVT 信封 | ✅ 已实施并真机验证（2026-10-26；double_click 待实按复核） |
| `upload.state` | 录音上传状态：`{name, state:"start"\|"done"\|"failed", bytes, error?}`。成功判定三重（2xx && 服务端 ok && size && sha256 一致），成功即记已传索引并**删源释放存储**；失败保留文件等重试；事件与协议 worker 解耦（BLE 断连丢事件不影响上传本体，upload.status 可查） | ✅ 已实施并真机验证（2026-10-27） |
| `ota.progress` | OTA 进度：`{state:"downloading"\|"verifying"\|"installing"\|"failed", progress?, bytes?, total?, error?}`。installing 发出后进程将被覆盖安装杀死、新版本自拉起（hello 回新版本号即成功）；failed 后设备存活，ota.status 查原因 | ✅ 已实施并真机验证（2026-10-27） |
| `sensor.data` | 传感器流帧（sensor.stream.start 开启）：`{values:{...同 sensor.read values 形状，仅含已交付类型}, interval_ms}`（周期性一帧合并多类型；500ms 下限节流；BLE 断连自动停流） | ✅ 已实施并真机验证（2026-10-30，批 2） |
| `location.fix` | 新定位到达（location.stream.start 开启）：`{lat, lon, accuracy_m?, altitude_m?, speed_mps?, bearing_deg?, satellites?, ts}`（仅新 fix 才发；BLE 断连自动停流） | ✅ 已实施并真机验证（2026-10-30，批 2） |
| *(随实施登记：battery.* 等)* | | |

### 9.3 待定项

- [x] BLE MTU 实测：macOS 连接协商到 **517**（2026-10-24，真机）——文件块大小按 MTU-3 自适应即可
- [x] 实时音频编码选型（2026-10-24 定稿）：**AMR-WB 12.65kbps（type=0x04）**。实测 SPRD 链路吞吐 ~3.6KB/s / ~11 包每秒（macOS 连接间隔 ~90ms，外设侧无 API 可调），16kHz ADPCM（8KB/s）不可行；AMR-WB 为 v2 同款、生态兼容、1650B/s 稳态无损。ADPCM（type=0x01）保留给快链路
- [ ] BLE 侧授权机制细节（配对码 vs token 命令下发）
- [ ] GATT Server 压测（133 频率、长时间反复连接稳定性）——MTU 协商与单连接已验证
- [ ] 云端 WS 消息信封细节（与 §4 映射的确认）
- [x] SDK 侧 AMR-WB → PCM 解码（2026-10-26）：vendor opencore-amrwb（Apache-2.0）+ FFI，`subscribe_audio()` 直接产出 16kHz 单声道 PcmChunk（与 reai-card-sdk 接口形态一致，供 driver-v2 实时消费）

## 10. 变更流程与兼容矩阵

- 变更流程：改本文档 → 评审 → app/SDK 同步实现 → 更新 §9.4 兼容矩阵 → CHANGELOG
- 不兼容变更（改已有命令语义/删命令）必须升 `proto_version` 主版本并在矩阵登记影响面
- 兼容矩阵（v1 起填）：

| proto_version | app 版本 | SDK 版本 | 小程序 | 备注 |
|---|---|---|---|---|
| 1 | 0.8.0（已实施） | 0.1.0（已实施） | 兼容 v2 帧 | 首版；文本帧 + 二进制类型头 + indexed 分片；音频 AMR-WB type=0x04 |

## 11. AP 附属 HTTP 端点（ap.start 激活期间，8788 端口）

AP 文件服务与命令模型（§4）并行：HTTP 不占用协议 worker，可与 BLE 命令同时使用。文件名一律强校验
`^audio_[0-9]{8}-[0-9]{6}(_[0-9A-F]{8}_[0-9]{2,})?\.amr$`（录音分段新旧命名，见 record.start），
拒绝任何路径分隔符（防目录穿越）；OTA 包路径除外（固定文件名，见下）。

| 端点 | 行为 |
|---|---|
| `GET /api/files` | `{files:[{name,bytes,mtime}]}`，仅 `files/audio/` 目录 |
| `GET /api/files/download?name=<name>` | `application/octet-stream` 流式下载；name 非法 403，不存在 404 |
| `POST /api/ota/upload` | OTA 安装包上传（暂存，不安装）：请求体为裸二进制 APK，头 `x-sha256`（Hex，可选，给则必比）、`x-size`（必填，须与 body 一致）；服务端流式落盘并计算 sha256，上限 64MB。成功 `{staged:true,size,sha256}` → 包暂存为 `files/ota/staged.apk`（跨重启保留），随后经 BLE `ota.install {confirm:true}` 确认安装（暂存包优先于 URL 清单源）；校验失败/写满即删除暂存 |

- 该服务无鉴权（AP 密码即边界）；生产部署若开放公网需在网关侧加鉴权（token 位已预留于上传/OTA 服务端）
- `ap.stop` 即停；已保存 STA 网络由系统自动恢复
