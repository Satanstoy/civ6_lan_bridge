# Civ6 LAN Bridge 协议基线

状态：Draft v0.1

本文件描述 Phase 0 固定的身份、端口和路由语义。HTTP API 的完整 OpenAPI 文件在 Phase 1 实现控制面时生成；本文件先固定不应被平台实现自行改变的规则。

## 1. 标识符

所有 ID 使用 UUID v4，服务端生成或验证：

| 标识 | 用途 |
| --- | --- |
| `room_id` | 服务端内部房间 ID |
| `room_code` | 用户输入的 6–12 位、大小写不敏感的 Crockford-like code |
| `peer_id` | 一台客户端设备在服务端的身份 |
| `host_session_id` | 一个 peer 当前 Civ6 房主会话 |
| `discovery_request_id` | 一次房间发现请求 |
| `gameplay_session_id` | 一个客户端到选定房主的游戏路由 |

`room_code` 只允许 `ABCDEFGHJKLMNPQRSTUVWXYZ23456789`，排除容易混淆的 `I`、`O`、`0`、`1`。服务端仍必须使用 `room_id` 做权限判断，不能把房间码直接当数据库主键。

## 2. Civ VI 端口

```text
discovery: 62900–62999/UDP
gameplay:  62056/UDP
```

服务端和客户端都必须拒绝其他 UDP 端口进入 Civ6 relay。单个 UDP datagram 的默认上限为 4096 字节；超过上限直接丢弃并计数，不分片、不改用 TCP。

## 3. 房间状态

```text
room created
    ↓
peer joined ── heartbeat ── peer active
    ↓
host registered ── heartbeat ── host active
    ↓
discovery request ── fan-out ── discovery responses
    ↓
host selected ── gameplay session ── UDP forwarding
    ↓
leave / timeout / relay restart ── session removed
```

默认生命周期：

- `host_session`：15 秒无心跳过期；
- `discovery_request`：5 秒过期；
- `gameplay_session`：30 秒无数据过期；
- relay 重启：所有内存 gameplay session 失效，客户端重新注册。

所有值由服务端返回的配置或能力文档决定，客户端不能自行延长。

## 4. Discovery 语义

客户端捕获 Civ VI 发往 `255.255.255.255:62900-62999` 的 discovery 包后，转换为服务端 relay 可以识别的请求。转换层只改变传输封装和目的地，不改 Civ VI payload。

服务端执行：

1. 验证 tunnel peer、room membership、端口、大小、频率和 token；
2. 创建 `discovery_request_id` 并保存客户端 peer；
3. 查找该 room 的所有未过期 `host_session`；
4. 把 discovery datagram fan-out 给每个 host；
5. 回包必须携带内部 `host_session_id`，再由客户端适配层恢复成对应房主的虚拟来源；
6. 迟于过期时间的回复丢弃。

一个 room 内可以同时有多个 host。房主身份必须贯穿 discovery 到 gameplay，不能把所有回复统一伪装成 relay 自己。

## 5. Gameplay 语义

客户端点击某个发现结果后，调用控制面创建 `gameplay_session_id`，并绑定：

```text
room_id
client_peer_id
host_session_id
host_peer_id
client_virtual_ip
host_virtual_ip
```

数据面的唯一允许方向是：

```text
client_peer_id → host_peer_id
host_peer_id   → client_peer_id
```

服务端不得根据客户端提交的任意公网 IP 做转发。服务端必须从已登记的 peer/virtual IP 表得到目的地址。来自第三个 peer 的包直接丢弃并计数。

## 6. 错误和重试

- 控制 API 写请求使用客户端生成的 idempotency key；
- UDP 数据包不重传，保持 Civ6 原有 datagram 语义；
- 控制面重试不得创建重复 room、host session 或 gameplay session；
- `401/403` 表示 token/权限问题，客户端需要重新鉴权；
- `404` 表示 session 已被清理，客户端需要重新发现；
- `409` 表示房间或 peer 状态冲突，客户端应刷新状态而不是盲目重试；
- 服务端重启后的旧 UDP 包全部无效。

## 7. 安全和隐私

- 私钥永不通过 API 传输；
- 控制面 token 只保存 hash；
- relay 日志只记录 room/peer/session ID、端口、长度、计数和时间，不记录 Civ6 payload；
- 所有输入执行长度、TTL、速率和并发限制；
- 任何跨 room 的 ID 组合都必须返回授权失败，不通过错误信息泄露其他房间状态。

## 8. 兼容性原则

协议层不依赖 Civ6 房间 JSON 字段。Civ6 版本更新时，优先验证：

1. discovery 端口范围是否变化；
2. gameplay 端口是否变化；
3. 客户端是否仍将广播和回包绑定到预期地址；
4. Civ6 是否增加了需要额外路由的 UDP 流量。

如果只发生 payload 变化，relay 不应因此需要升级；如果端口或地址语义变化，必须增加协议版本和兼容测试。

## 9. 当前 relay envelope

服务端 `server/src/relay.rs` 使用一个小型、版本化的 UDP envelope 承载 WireGuard 内的客户端适配器流量。它不是 Civ VI 自定义 payload，也不要求服务端解析 Civ VI 内容。固定头部为：

```text
magic[4] = "C6LB"
version[1] = 1
kind[1]
body_len[2] = big-endian
```

当前消息方向：

| kind | 方向 | 关键字段 |
| --- | --- | --- |
| `DiscoveryRequest` | client → server | request ID、Civ6 discovery 端口、原始 payload |
| `DiscoveryToHost` | server → host adapter | request ID、客户端虚拟源 IP、目的端口、原始 payload |
| `DiscoveryResponse` | host adapter → server | request ID、host session ID、来源端口、原始 payload |
| `DiscoveryToClient` | server → client adapter | request ID、房主虚拟源 IP、来源端口、原始 payload |
| `GameplayPacket` | client/host adapter → server | gameplay session ID、来源端口、原始 payload |
| `GameplayToPeer` | server → client/host adapter | gameplay session ID、虚拟源 IP、目的端口、原始 payload |
| `RelayProbe` | client adapter → server | request ID；验证 datagram transport 到 relay 的有效交换 |
| `RelayProbeAck` | server → client adapter | 对应 request ID；不代表 Civ6 已通过 2K 年龄验证 |

服务端只接受客户端方向的 `DiscoveryRequest`、`DiscoveryResponse`、`GameplayPacket` 和 `RelayProbe`；收到 outbound kind、未知 peer、伪造 host session、错误端口或超过 4096 字节的 payload 时直接丢弃。默认 envelope relay 端口为 `32000/UDP`，而 Civ VI 的 `62900-62999/UDP` 与 `62056/UDP` 只出现在 envelope 字段中。

客户端和服务端必须从共享协议 crate 使用同一套 envelope 编解码。`RelayClient` 只依赖 `DatagramTransport` 抽象，UDP 是默认实现；QUIC DATAGRAM、udp2raw 等候选传输必须保持相同的消息边界和超时语义，不能改变房间路由。
