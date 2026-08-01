# Civ6 LAN Bridge 工程规格

状态：Draft v0.1

日期：2026-08-02

本文是三端工程的总规格。它把“服务器承载全部网络服务、任意成员可以开房、Windows 和 macOS 一键接入”拆成可以实现和验收的系统边界。除非后续 ADR 明确修改，本文件中的决策是实现基线。

## 1. 产品定义

### 1.1 目标

Civ6 LAN Bridge 是一个 Civ VI 专用的跨网络 LAN 连接服务。玩家仍在自己的 Windows 或 macOS 电脑上运行正版 Civ VI，但以下网络能力由本服务集中提供：

- 设备注册、房间码、成员鉴权和短期会话；
- Civ VI 房间发现请求的集中接收和 fan-out；
- Civ VI 游戏 UDP 流量的服务器中继；
- 虚拟地址、房主会话和房间隔离；
- 隧道保活、断线重连、过期会话清理和可观测性。

网络上不再依赖 `255.255.255.255` 能否跨路由器传递，也不要求用户向家庭路由器配置入站端口转发。

### 1.2 必须诚实的边界

本服务不是 Civ VI Dedicated Server，也不运行 Civ VI 的游戏逻辑。房主的 Civ VI 进程仍在玩家电脑上运行，因此以下问题不由本服务解决：

- 房主电脑崩溃、休眠、掉线或游戏进程退出；
- Civ VI 版本、DLC、Mod、地图或规则不一致；
- Civ VI 本身的游戏同步错误、存档损坏或游戏 Bug；
- 需要官方无界面 Civ VI 服务端才能实现的服务器权威模拟。

本项目的稳定性承诺是“网络服务稳定、路由稳定、房间发现稳定”，不是替代 Civ VI 游戏主机。

还有一个必须显式暴露给用户的外部依赖：Civ VI 房主仍需要能够连接 2K Online Services 完成账号/年龄验证。已有 Steam 社区案例显示，2K 验证不可用时，即使选择 LAN，游戏也可能在“确认设置”阶段不继续创建房间。因此客户端不能承诺绕过 2K 检查；它必须把“2K 验证失败”和“Civ6 LAN Bridge relay 失败”区分展示。房主完成 2K 验证后，本服务才负责后续的发现、路由和 UDP 中继。

### 1.4 规模边界和交付档位

首个可交付版本的目标规模明确为：单台 VPS、一个 Rust relay 进程、2–10 名玩家、少量并发房间。这个规模下，服务端的权威运行状态保存在内存即可；PostgreSQL 不是启动前置条件，未配置数据库时允许服务正常提供控制面和 relay，但重启后客户端必须重新加入房间并重建 host/gameplay session。

MVP 控制面使用部署环境注入的一枚 bearer token，便于朋友组网和单 VPS 部署；这不是最终的多租户身份模型。Production profile 才引入每设备身份、短期 room/join token、token hash、撤销和审计生命周期，不能把当前 MVP token 设计误报成完整账号系统。

PostgreSQL、节点调度、主备 relay、完整设备生命周期和可观测性增强属于 Production profile，不作为朋友组网 MVP 的验收门槛。多房间、多个房主和房间隔离仍属于 MVP 必需能力，因为它们直接决定“任意成员开房时房间是否可见、是否串线”，不是为了追求大规模吞吐。

### 1.3 非目标

- 不做通用 VPN；
- 不做二层以太网桥接或把用户家庭 LAN 暴露给其他成员；
- 不解析或修改 Civ VI 房间 JSON 业务内容；
- 不实现 Civ VI 游戏逻辑；
- 不在服务端执行未经授权的 Civ VI 客户端；
- 不默认依赖 `udp2raw`、TCP 伪装或不透明的第三方转发层。

## 2. 核心网络模型

### 2.1 为什么不用二层桥接

Civ VI 的发现流程使用 IPv4 limited broadcast，并在 `62900-62999/UDP` 范围内发送发现包；实际游戏流量使用 `62056/UDP`。广播地址不会被普通三层路由器转发，因此仅把玩家放进同一个 WireGuard L3 网段不能保证 LAN 列表可见。

解决方式不是把所有用户的二层网络桥接起来，而是由客户端识别 Civ VI 的发现数据，把广播转换成发往服务端的专用请求；服务端再向当前房间内所有有效房主 fan-out。游戏数据则通过房间/房主路由表转发。

参考：

- [Civ VI discovery protocol analysis](https://github.com/xaxys/injciv6)
- [Microsoft UDP broadcast behavior](https://learn.microsoft.com/en-us/dotnet/framework/network-programming/using-udp-services)

### 2.2 目标拓扑

```text
Windows Civ6 ─┐                         ┌─ Civ6 host process
               ├─ platform adapter ─────┤
macOS Civ6 ────┘        encrypted       │
                         tunnel         │
                       ┌─────────┐      │
                       │ Server  │──────┘
                       │         │
                       │ API     │  room registry
                       │ Relay   │  discovery fan-out
                       │ Routing │  62056 UDP forwarding
                       └─────────┘
```

服务器是所有客户端的网络汇聚点。客户端之间不建立直接的 Civ6 数据连接；服务端对每个房间保持独立的路由状态。

### 2.3 房间和房主模型

一个 `room` 可以有多个在线成员和多个短期 `host_session`。房间本身不绑定固定房主。任意成员点击 Civ VI 创建房间后，客户端把自己登记为该房间的当前房主。

关键对象：

| 对象 | 作用 | 生命周期 |
| --- | --- | --- |
| `room` | 房间隔离边界和人类可读房间码 | 创建到过期 |
| `peer` | 一个已鉴权设备在房间中的网络身份 | 加入到离开 |
| `host_session` | 一个 peer 当前可被发现和连接的 Civ6 房主实例 | 心跳维持，短期过期 |
| `discovery_request` | 一次 Civ6 房间发现请求 | 数秒 |
| `gameplay_session` | 一个客户端到选定房主的 62056 路由 | 游戏期间 |

必须使用以下逻辑路由键，不能使用单一全局房主 IP：

```text
(room_id, discovery_request_id, host_peer_id) -> response destination
(room_id, gameplay_session_id) -> host_peer_id
```

房主断线或心跳超时后，所有相关 `host_session` 和 `gameplay_session` 都必须失效。

## 3. 技术栈决策

### 3.1 总体原则

- 网络面优先使用已有标准协议，不自定义加密算法；
- Civ6 数据面保持 UDP datagram 语义，不用 TCP 直接承载游戏包；
- 控制面和数据面分离；
- 先做单活中继和可恢复状态，再扩展多节点；
- 每个外部输入都要有大小、频率、生命周期和权限上限；
- 版本、工具链、依赖和发布制品全部可复现。

### 3.2 服务端

| 层 | 选择 | 说明 |
| --- | --- | --- |
| 语言 | Rust stable | 内存安全、适合长期运行的 UDP 服务；通过 `rust-toolchain.toml` 锁定版本 |
| 异步运行时 | Tokio | UDP socket、定时器、任务取消和并发控制 |
| HTTP API | Axum + Tower/Tower-HTTP | 控制面路由、中间件、超时、限流、鉴权和 tracing |
| 序列化 | serde + JSON | 控制 API 可调试、可审计；所有请求带版本字段 |
| 数据库 | PostgreSQL 18，锁定精确 minor 版本（Production profile） | 房间、peer、token 哈希、host session、审计记录和节点分配 |
| 数据库访问 | SQLx + migration（Production profile） | 编译期检查查询，迁移进入 Git；MVP 未配置数据库时使用内存状态 |
| Relay 状态 | 内存有界状态机 | 每个 UDP 包不查数据库；数据库只保存控制面状态和恢复所需信息 |
| 可观测性 | tracing + OpenTelemetry/Prometheus | 结构化日志、指标、分布式 trace；不记录游戏 payload |
| 进程管理 | systemd | 自动重启、资源限制、日志、启动顺序和健康检查 |
| 防火墙 | nftables | 只开放控制面和隧道入口，禁止开放式 UDP reflector |
| TLS 边缘 | Caddy 或云负载均衡 | 控制 API 终止 HTTPS；relay/隧道端口单独管理 |

Axum 官方文档说明其基于 Tower 生态，可直接复用超时、授权、压缩和 tracing 等中间件；PostgreSQL 官方文档提供 streaming replication 和 hot standby，可作为后续控制面高可用基础。

参考：[Axum](https://docs.rs/axum/latest/axum/index.html)、[PostgreSQL high availability](https://www.postgresql.org/docs/current/high-availability.html)、[OpenTelemetry Rust](https://opentelemetry.io/docs/languages/rust/)。

### 3.3 隧道和数据面

第一版采用 **WireGuard L3 隧道作为默认安全传输**，服务端使用 Linux WireGuard 接口，客户端为每台设备分配唯一虚拟地址。WireGuard 负责加密、peer 身份、NAT 后连接和网络路径；Civ6 专用 relay 负责广播转换、房间 fan-out 和房主路由。

不把 WireGuard 当成“自动传递 LAN 广播”的方案。广播仍然必须在客户端转换成服务端可路由的 Civ6 discovery 请求。

**Phase 1 硬约束：relay 协议必须建立在抽象的 datagram transport 之上。** 传输接口从第一天就固定 datagram 边界、最大 MTU、丢包/乱序/超时语义和诊断指标；默认实现是 WireGuard/UDP，后续可以插入 QUIC DATAGRAM、udp2raw 或其他 UDP/443 兼容实现，而不能改写房间协议，也不能把 Civ6 gameplay 降级成 TCP 字节流。实现 fallback 可以后置，但抽象不能后置。

UDP QoS/丢包不是 edge case。受限运营商网络可能导致 WireGuard UDP 被限速，继而出现 Civ6 “Player data is out of sync” 或多人局反复掉线。QUIC DATAGRAM 或 udp2raw 只有在真实丢包、抖动、MTU 和 6 人长局压测中证明改善后，才可作为稳定版 transport；不能仅凭“端口能通”纳入默认路径。QUIC DATAGRAM 是 RFC 9221 标准中的不可靠 datagram 扩展，仍受拥塞控制、MTU 和丢包影响。

参考：[WireGuard](https://www.wireguard.com/)、[RFC 9221 QUIC DATAGRAM](https://www.rfc-editor.org/rfc/rfc9221.html)。

### 3.4 Windows 客户端

Windows 10+ 客户端由 Tauri 2 + Rust core + TypeScript UI 组成，分为普通 UI 进程和高权限网络服务两部分：

- Tauri UI：登录/房间码、连接状态、诊断、更新和日志导出；
- Rust core：控制 API、WireGuard 配置、状态机和本地 IPC；
- 网络适配器：负责 Civ6 discovery 广播转换和必要的 UDP 地址注入；
- Windows service：以最小必要权限运行网络适配器，不把管理员权限留在 UI 进程。

生产适配器必须使用 **WFP 出站传输层拦截**，不是被动监听虚拟网卡：对 Civ6 进程发往 `255.255.255.255:62900-62999/UDP` 的 outbound 广播进行过滤，在 callout 中把目标改写为 relay/隧道目标并封装原始 payload；回程按房主虚拟身份恢复为 Civ6 可接受的本地来源。`62056/UDP` 也必须按同一会话策略处理。过滤器必须按进程、协议、端口和方向收窄，禁止截获其他应用流量。微软 WFP 的 transport layer/callout 和 packet injection 能力是该设计的依据。

被动监听虚拟网卡或只做广播转发不算生产实现；它无法可靠截获 Civ6 已经发出的 limited broadcast，也无法保证回复重新进入正确的 Civ6 socket。

如果第一阶段采用 WinDivert 交付，必须把它标记为兼容实现而不是最终架构，并固定版本、校验签名、限制过滤器范围，只捕获 Civ6 相关 UDP 流量。安装器/Windows service 必须自动创建和回收最小范围的 Windows Firewall 规则；用户不应遇到“能 ping 通但搜不到”的手动防火墙配置陷阱。

参考：[Microsoft WFP](https://learn.microsoft.com/en-us/windows/win32/fwp/windows-filtering-platform-start-page)、[Microsoft WFP traffic inspection sample](https://learn.microsoft.com/en-us/samples/microsoft/windows-driver-samples/windows-filtering-platform-traffic-inspection-sample/)、[WinDivert](https://github.com/basil00/WinDivert)。

### 3.5 macOS 客户端

macOS 客户端由 Tauri 2 + Rust core + Swift `NEPacketTunnelProvider` system extension 组成：

- Tauri UI 负责用户交互和状态展示；
- Swift Network Extension 负责创建虚拟接口、读取/写入 packet flow 和生命周期管理；
- Rust core 负责控制 API、会话状态、协议编码和诊断；
- 只把 Civ6 虚拟地址/指定路由送入隧道，不接管用户全部互联网流量；
- 对 `255.255.255.255` discovery 包在本地转换后再送入服务端，不把不可路由广播直接发送到隧道。

Apple 的 TN3120 明确支持 packet tunnel 用于把网络流量送入远端安全网络，但不建议用它在本地充当 listener/proxy。因此本项目把它实现为“到远端 relay 的包隧道”，不把 packet provider 设计成通用本地 UDP 代理。macOS 直接分发使用 system extension、Developer ID 签名和公证；Network Extension entitlement 是发布前置条件。由于 Aspyr macOS 移植版历史上存在跨平台联机差异，Phase 3 的真实 Apple Silicon 设备测试权重高于模拟器、单机或仅探针测试，必须包含 Mac↔Windows、Mac↔Mac、2K 验证和多人长局。

参考：[NEPacketTunnelProvider](https://developer.apple.com/documentation/networkextension/nepackettunnelprovider)、[TN3120](https://developer.apple.com/documentation/technotes/tn3120-expected-use-cases-for-network-extension-packet-tunnel-providers)、[TN3134](https://developer.apple.com/documentation/technotes/tn3134-network-extension-provider-deployment)。

### 3.6 桌面 UI 和安装包

- Tauri 2；
- Rust 后端；
- TypeScript + Vite shared UI package；
- Windows 输出 x64 NSIS `.exe`，后续可追加 `.msi`；
- macOS 输出 Apple Silicon ARM64 `.dmg`，候选包必须把 Packet Tunnel `.appex` 放入 App 的 `Contents/PlugIns`；
- Windows 安装器、服务、驱动/组件都要签名；
- Windows 安装器/服务自动放行 relay UDP、WireGuard 和 Civ6 相关的最小规则范围，并在卸载时清理规则；
- macOS App、Network Extension 和 DMG 都要签名，并使用 `notarytool` 公证和 staple；
- 更新器只接受签名 manifest 和签名制品，不能执行任意远程文件。

Tauri 官方支持 Windows NSIS 安装器和 macOS DMG；macOS 直接分发需要代码签名和公证。

参考：[Tauri distribution](https://v2.tauri.app/distribute/)、[Tauri Windows installer](https://v2.tauri.app/distribute/windows-installer/)、[Tauri macOS bundle](https://v2.tauri.app/distribute/macos-application-bundle/)。

## 4. 服务端组件规格

### 4.1 进程划分

正式服务端至少包含以下逻辑模块，第一版可以编译为一个进程：

```text
server/
├── api             # HTTPS 控制面
├── auth            # device/room/session token
├── allocator       # room、peer、virtual IP 分配
├── wireguard       # peer 配置和隧道状态
├── discovery       # Civ6 62900-62999 请求 fan-out
├── relay           # Civ6 UDP datagram 路由
├── health          # readiness/liveness/metrics
└── persistence     # PostgreSQL repository + migrations
```

控制面和数据面共享鉴权后的内存状态，但数据面不能依赖每个包都访问 PostgreSQL。

### 4.2 控制 API

API 使用 `/v1` 前缀，JSON 请求和响应必须带 `request_id`、`server_time` 或等价诊断字段。初版接口：

```text
POST   /v1/devices/register
POST   /v1/rooms
POST   /v1/rooms/{code}/join
GET    /v1/rooms/{code}/status
POST   /v1/rooms/{code}/hosts
POST   /v1/rooms/{code}/heartbeat
POST   /v1/rooms/{code}/gameplay-sessions
DELETE /v1/rooms/{code}/hosts/{host_session_id}
DELETE /v1/rooms/{code}/peers/{peer_id}
GET    /health/live
GET    /health/ready
GET    /metrics
```

规则：

- 所有写操作幂等键有效期至少覆盖一次网络重试窗口；
- 房间码使用安全随机源生成，不能使用递增 ID；
- room code、join token、device token 分开；
- token 数据库存哈希，不保存可直接登录的明文 token；
- host session 默认 15 秒无心跳过期，实际值可配置；
- discovery request 默认 5 秒过期；
- gameplay session 在空闲超时或成员离开时清理；
- API 对设备注册、加入房间、心跳、创建 host session 分别限流；
- 错误响应不泄露房间是否存在给未授权请求者。

### 4.3 Relay 行为

#### Discovery

1. 客户端把 Civ6 发往 `255.255.255.255:62900-62999` 的包转换为带本地 peer 身份的 discovery datagram；
2. 服务端验证 peer、room、端口、请求大小和速率；
3. 服务端将请求 fan-out 到该 room 当前有效的 host sessions；
4. 每个房主的回包必须绑定 `host_peer_id`，不能统一伪装为 relay 的唯一房主；
5. 客户端保留房主虚拟地址/标识，用户选择后创建 gameplay session；
6. request 超时后丢弃所有迟到回包。

#### Gameplay

1. 客户端只允许向选定的 `host_peer_id` 建立 `62056/UDP` gameplay session；
2. 服务端校验 `(room_id, client_peer_id, host_peer_id, session_id)`；
3. 数据面只转发 Civ6 允许的 UDP 端口和最大 datagram 大小；
4. 不允许把未授权目的地址变成任意公网 UDP 上游；
5. 服务器为每个 session 统计包数、字节数、丢弃数和最后活动时间；
6. 服务器重启后不恢复旧 gameplay session，客户端必须重新鉴权和重建会话。

### 4.4 虚拟地址和路由

每个 peer 分配不可重复的虚拟 IPv4 地址，地址只在服务端管理的 tunnel/relay 域内有效。客户端不能自行声明虚拟源地址；服务端必须校验 WireGuard peer public key 与分配地址的绑定。

推荐第一版使用单房间节点亲和性：一个 room 的所有 peer 和 host sessions 由同一个 active relay 处理。多节点时使用一致性分配或控制面分配，不使用无状态 UDP 负载均衡把同一房间随机打散。

## 5. 安全规格

### 5.1 身份和密钥

- 设备首次运行生成本地密钥；
- WireGuard private key 只存在客户端安全存储；
- 控制面身份密钥与 WireGuard key 分离；
- Windows 使用 DPAPI/凭据管理器，macOS 使用 Keychain；
- 服务端只保存 public key 和 token hash；
- room token 只授予指定房间成员权限，不能调用管理 API；
- 服务器管理员凭据不进入客户端包或仓库。

### 5.2 Relay 防滥用

- 默认拒绝所有未登记的 UDP peer；
- 只允许 Civ6 需要的端口范围；
- 限制单 peer、单 room、单 relay 的包速率、字节速率和并发 session 数；
- 限制发现 fan-out 的房主数量和请求频率；
- 严格限制 datagram 最大长度；
- 过期 session 立即释放 socket/映射；
- 记录元数据审计，不记录 Civ6 payload；
- 房间之间做正向隔离测试，不能只依赖客户端 UI 隐藏。

### 5.3 客户端权限

UI 不以管理员/root 身份运行。需要安装驱动、服务或 Network Extension 时由安装器/系统授权流程完成；网络服务通过受限 IPC 接受 UI 命令，并验证调用方身份和命令参数。

## 6. 稳定性和高可用

### 6.1 第一阶段：单活中继

MVP 的可靠基线是：

- 一台固定公网 IP 的 Ubuntu LTS relay 节点；
- 控制 API HTTPS 与隧道入口分离；
- 使用有界内存状态；若启用 PostgreSQL，则使用独立数据卷、自动备份和恢复演练；
- relay 由 systemd 管理，异常退出自动重启；
- 客户端有控制面、隧道、relay、Civ6 discovery 四个独立状态；
- 中继重启后客户端自动重新注册，不恢复旧 UDP session；
- 运行时状态有界，不能因无效公网包无限增长。

### 6.2 第二阶段：主备和多节点

服务规模扩大后再增加：

- PostgreSQL streaming replication + hot standby；
- 两个 relay 节点的 active/standby；
- 固定入口或云厂商 UDP 负载均衡；
- room 粘性路由；
- 节点失效后的 room 重新分配和客户端重连；
- 节点容量和区域选择。

不能把“两个 relay 都监听同一 UDP 端口”误认为高可用。没有 session 粘性和路由状态复制时，随机负载均衡会造成房间发现和游戏包串线。

### 6.3 目标指标

以下是第一版验收目标，不是对任意公网环境的绝对保证：

| 指标 | 目标 |
| --- | --- |
| 房间隔离 | 未授权 peer 看到其他房间：0 次 |
| discovery | 同地区、正常网络下 p95 在 2 秒内完成 |
| relay 处理 | 服务端本地转发额外处理延迟 p99 小于 5 ms |
| 断线恢复 | 短暂网络切换后 10 秒内恢复控制面并可重新发现 |
| 进程恢复 | relay 进程崩溃后由 systemd 自动拉起 |
| 资源上限 | 每个 room、peer、session、IP 入口均有明确上限 |

真实用户网络、运营商 UDP 丢包和房主电脑状态必须单独记录，不能混入服务端 SLO。

## 7. 仓库结构

```text
civ6-lan-bridge/
├── server/                 # Rust 服务端控制面、WireGuard 管理和 UDP relay
├── win-client/             # Windows Tauri + Rust + WFP outbound adapter
├── mac-client/             # macOS Tauri + Swift Network Extension
├── clients/ui/             # Windows/macOS 共用的 TypeScript UI 包
├── crates/
│   ├── protocol/           # 三端共享的版本化数据结构和错误码
│   └── router/             # 多房间、多房主、session 路由状态机
├── docs/
│   ├── spec.md
│   ├── architecture.md
│   ├── deployment.md
│   ├── protocol.md
│   └── adr/
├── tests/                  # relay、协议和跨平台集成测试
├── config/                 # 仅示例配置，无密钥
├── systemd/                # 服务单元和 hardening 配置
└── .github/workflows/      # CI、签名和发布
```

三个可交付项目仍然是 `server/`、`win-client/`、`mac-client/`。`crates/` 只是共享协议库，不改变三个项目边界。

## 8. 实现阶段

### Phase 0：网络和协议基线

- 把当前 Python relay 标为 prototype；
- 建立 Rust workspace 和 `crates/protocol`；
- 固化 Civ6 端口、discovery request 生命周期和路由键；
- 用伪造 UDP 客户端覆盖多 room、多 host、多请求并发；
- 完成 malformed packet、放大攻击和跨房间隔离测试；
- 固化 `DatagramTransport` 抽象，至少完成 UDP 实现、丢包/乱序/超时可观测字段和 fallback 协议接口。

### Phase 1：Rust 服务端单活版

- Axum 控制 API；
- PostgreSQL migration；
- WireGuard peer/虚拟 IP 管理；
- 多 room、多 peer、多 host session relay；
- Prometheus/OpenTelemetry 指标；
- systemd/nftables 生产部署；
- API、relay 和恢复集成测试。

当前已完成 Phase 1 的服务端核心和共享 relay envelope；客户端 core 已经把 relay 建立在可替换的 `DatagramTransport` 接口上，并有 UDP probe 联调测试。尚未完成 Windows WFP 出站 callout、macOS Network Extension 实机注入和生产指标/限流集成。

### Phase 2：Windows 客户端

- Tauri UI 和 Rust 状态机；
- WireGuardNT/隧道生命周期；
- WFP packet adapter；
- WinDivert 兼容模式仅用于早期验证；
- Windows service、安装器、代码签名和回滚；
- Windows 10/11 实机发现和 62056 联机测试。

### Phase 3：macOS 客户端

- Tauri UI 和 Swift Network Extension；
- PacketTunnel 只转发指定虚拟路由；
- Apple Silicon ARM64 构建；
- Developer ID、entitlements、notarization、staple；
- macOS Apple Silicon 实机测试。

### Phase 4：生产化和高可用

- 主备 relay；
- PostgreSQL 复制和恢复演练；
- 节点容量、房间粘性、区域调度；
- 灰度升级、客户端自动更新和签名 manifest；
- 真实家庭宽带、手机热点、校园/公司网络测试。

## 9. 验收标准

一个版本只有同时满足以下条件才可称为可用版本：

1. Windows 10/11 x64 与 macOS Apple Silicon 均可安装并完成鉴权；
2. 两个不同网络的玩家加入同一个 room code；
3. 任意一方创建 Civ VI 房间，另一方能在 Civ VI LAN 列表看到；
4. 房间内其他成员创建的房间也能被正确区分和加入；
5. 两个 room code 之间互相不可见、不可加入、不可转发；
6. 发现包和 `62056/UDP` 均经过服务器，而不是客户端直接互连；
7. 房主退出后，旧 host session 在过期窗口内消失；
8. relay 重启后，客户端能自动重新注册并重新发现；
9. 注入恶意来源地址、错误端口、超大 datagram 和过期 token 都被拒绝；
10. Windows 安装器自动配置并在卸载时清理防火墙规则；
11. 房主能够连接 2K Online Services 完成年龄验证；验证不可用时客户端明确提示外部依赖，而不是声称 relay 已损坏；
12. 在受限 UDP 网络、丢包和抖动条件下，基线 UDP 与候选 fallback 都有可复现的指标和 6 人长局结果；
13. 发布物具有可验证签名、SHA-256、版本号和对应 Git commit。

## 10. 已确认的外部最佳实践

本规格的关键技术约束来自以下官方或标准资料：

- [Axum documentation](https://docs.rs/axum/latest/axum/index.html)：基于 Tokio/Hyper，并复用 Tower 中间件；
- [Tokio](https://tokio.rs/)：Rust 异步网络运行时；
- [WireGuard](https://www.wireguard.com/)：加密 L3 tunnel 和 peer 公钥身份；
- [Microsoft WFP](https://learn.microsoft.com/en-us/windows/win32/fwp/windows-filtering-platform-start-page)：Windows 现代网络过滤/修改平台；
- [Microsoft WFP basic operation](https://learn.microsoft.com/en-us/windows/win32/fwp/basic-operation) 与 [packet injection functions](https://learn.microsoft.com/en-us/windows-hardware/drivers/network/packet-injection-functions)：出站传输层 callout、包修改和重新注入依据；
- [Apple TN3120](https://developer.apple.com/documentation/technotes/tn3120-expected-use-cases-for-network-extension-packet-tunnel-providers)：packet tunnel 的适用边界；
- [Apple NEPacketTunnelProvider](https://developer.apple.com/documentation/networkextension/nepackettunnelprovider) 与 [NEPacketTunnelFlow](https://developer.apple.com/documentation/networkextension/nepackettunnelflow)：虚拟接口和 packet flow 生命周期；
- [Apple TN3134](https://developer.apple.com/documentation/technotes/tn3134-network-extension-provider-deployment)：Network Extension 部署形式和系统版本约束；
- [Tauri distribution](https://v2.tauri.app/distribute/)：Windows 安装器、macOS DMG、签名和公证；
- [RFC 9221](https://www.rfc-editor.org/rfc/rfc9221.html)：QUIC DATAGRAM 的不可靠 datagram 语义；
- [PostgreSQL high availability](https://www.postgresql.org/docs/current/high-availability.html)：复制、主备和 hot standby 的后续演进路径；
- [OpenTelemetry Rust](https://opentelemetry.io/docs/languages/rust/)：Rust 的 metrics、logs 和 traces 可观测性方向。
- [Steam Civ VI LAN discussion](https://steamcommunity.com/app/289070/discussions/4/3841053719663503593/)：2K 年龄验证不可用时 LAN 创建可能卡在确认设置的现实案例。

## 11. 当前实现差距

截至本文日期，仓库中已经有可测试的 Python 早期 relay、Rust 服务端控制面/路由核心、PostgreSQL repository、共享 relay envelope、共享 datagram client core 和 Tauri 诊断客户端，但尚未完成：

- Windows WFP 出站传输层 callout/packet injection 适配器；
- macOS Network Extension 的生产签名实机包、Tauri bundle 嵌入和 packet injection；
- Windows/macOS 端完整的 WireGuard 生命周期、Civ6 discovery/gameplay 注入和会话控制 UI；
- 签名的 `.exe`/`.dmg` 发布流水线（当前 CI 只能生成未签名候选制品）；
- Prometheus/OpenTelemetry 指标、速率限制和公网生产防护；
- 双端真实设备验收。

Phase 0 已完成的内容：

- 根目录 Rust workspace、stable toolchain 配置和锁定的 `Cargo.lock`；
- `crates/protocol`：Civ VI 端口、房间码、peer/session ID 和虚拟 IP 类型；
- `crates/router`：多房间、多房主、discovery fan-out、双向 gameplay 路由和 TTL 清理；
- `docs/protocol.md`：客户端与服务端共享的路由/错误/生命周期基线；
- 28 个 Rust 单元测试覆盖端口校验、房间隔离、房主身份、双向转发、过期清理、relay envelope 编解码、来源身份校验、共享 codec 兼容性、真实 client-core HTTP/UDP 控制面与数据面交换、MVP 内存 readiness、重复房间和跨房间授权。

因此本文件是正式实现的基线，不代表所有功能已经完成。下一步是完成三个项目目录的 Windows/macOS 客户端适配器，并在真实宽带、热点和受限网络中验证 discovery 与 `62056/UDP` 的端到端行为。
