# Server

服务端负责三件事：

1. WireGuard 私网和客户端 peer 管理；
2. Civ VI `62900-62999/UDP` 房间发现及 `62056/UDP` 游戏流量 relay；
3. 房间码、在线客户端和动态 host session 的控制面。

仓库里的 [`civ6-relay.py`](civ6-relay.py) 是早期单房主网络路径原型，不能作为生产 systemd 入口。正式 Rust 服务端不再把房主写死在环境变量里，而是按 room、peer 和 host session 路由。

正式技术基线见 [`docs/spec.md`](../docs/spec.md)。2–10 人 MVP 使用 Rust stable、Tokio、Axum、内存路由状态、systemd 和 nftables；PostgreSQL/SQLx 作为 Production profile 的可选持久化层。当前 Python 文件只用于网络路径验证，不是正式多房间服务端。

Phase 1 已加入 Rust 控制面入口和 WireGuard peer 命令适配：

    GET    /health/live
    GET    /health/ready
    POST   /v1/rooms
    POST   /v1/rooms/{code}/join
    GET    /v1/rooms/{code}/status
    POST   /v1/rooms/{code}/hosts
    POST   /v1/rooms/{code}/heartbeat
    POST   /v1/rooms/{code}/gameplay-sessions
    DELETE /v1/rooms/{code}/hosts/{host_session_id}
    DELETE /v1/rooms/{code}/peers/{peer_id}

所有 /v1 接口要求 Authorization: Bearer ...。设置 `CIV6_DATABASE_URL` 后，房间、peer、host session 和 gameplay session 的控制面 mutation 会写入 PostgreSQL；启动时恢复房间、peer、虚拟 IP 和 WireGuard peer，旧 host/gameplay session 会被清理并要求客户端重新建立。

## 任意成员开房的路由模型

一个房间不能只有一个固定 `CIV6_HOST_WG_IP`。正确模型是：

1. 房间内每个在线 peer 都可以成为 Civ VI 房主；
2. 客户端刷新时，服务端把发现请求 fan-out 到房间内其他 peer；
3. 每个开房 peer 的回复都要带上自己的 `host_peer_id` 和虚拟 IP；
4. 客户端必须保留不同房主的来源地址，用户点击某个房间时，`62056/UDP` 连接到对应房主；
5. 房主退出或切换时，通过 heartbeat 注销旧 host session。

不能简单地把所有回复都从同一个 relay IP 发回客户端，否则多个房间虽然可能显示出来，加入时却无法区分应该把 `62056/UDP` 发给哪个房主。

推荐的 MVP 实现：

- Rust stable；
- Tokio：异步 UDP、定时器和连接管理；
- Axum：HTTPS 控制 API；
- systemd：服务守护、自动重启和日志；
- nftables：只允许 WireGuard/relay 的必要端口。

启用 Production profile 后再加入 PostgreSQL、节点调度、主备 relay 和完整可观测性。

WireGuard 是默认的加密 L3 传输。Phase 1 已将 relay 放在可替换的 datagram transport 抽象之后；`udp2raw`/QUIC 只有在实网测试确认某些网络阻断 WireGuard UDP 且 Civ6 长局指标改善后，才能作为独立 fallback 评估。

Windows 正式客户端按 Microsoft Windows Filtering Platform 设计；WinDivert 仅作为当前原型/兼容实现。macOS 正式客户端使用 `NEPacketTunnelProvider` 把 Civ VI 相关流量送入远端 relay，不把 Network Extension 做成本地通用 UDP proxy。

部署时只把控制 API 放在 HTTPS 后面；Civ VI UDP relay 只接受 WireGuard peer 来源，不暴露为公网开放 UDP 转发器。

Rust 数据面已提供绑定 `CIV6_RELAY_BIND` 的真实 UDP envelope relay。它不直接转发公网任意 UDP，而是只接受 WireGuard 虚拟地址对应的已登记 peer，并将 `request_id`、`host_session_id`、`gameplay_session_id` 和虚拟源地址带到客户端适配器。共享 client core 已通过真实 UDP socket probe 测试；systemd 应启动 `/usr/local/bin/civ6-lan-server`，而不是早期 Python prototype。Windows WFP 和 macOS Network Extension 的 Civ6 注入适配器尚未完成，因此当前仍不能宣称已经交付可直接进行 Civ6 联机的 `.exe`/`.dmg`。

服务端可重复的 macOS transport-level runner 是 `scripts/mac-e2e-server-test.sh`，它启动正常 Rust 服务、生成临时 Bearer manifest、执行认证 UDP/房间 fan-out/隔离/TTL/错误路径测试，并生成脱敏的 `server-test-report.json`。没有真实第二个 Civ VI 客户端时，报告状态只能是 `partial`，`civ6_discovery` 必须保持 `not_tested`。

relay envelope 的字段和方向见 [`docs/protocol.md`](../docs/protocol.md)；默认服务端 relay 端口为 `32000/UDP`，Civ VI 原始端口仍只允许 `62900-62999/UDP` 和 `62056/UDP`。
