# Production deployment

本文分为“当前 relay 原型上线”和“2–10 人 MVP/Production profile”两阶段。MVP 不要求 PostgreSQL；不要把实验配置直接暴露到公网。

## 1. Server baseline

正式第一阶段建议使用一台 Ubuntu LTS 云服务器，按单活 relay 部署：

- 固定公网 IPv4，最好同时提供 IPv6；
- 至少 1 vCPU / 1 GB RAM；
- 系统时间同步；
- 独立域名，例如 `civ6.example.com`；
- 管理 SSH 只允许固定管理来源；
- 公网只开放控制 API HTTPS 和 WireGuard UDP 入口，Civ VI relay 只绑定 `wg0`；`udp2raw` 不是默认入口。
- 控制 API 由 Caddy/云负载均衡终止 HTTPS，Rust 服务只监听本机回环地址。
- Production profile 的 PostgreSQL 使用独立数据卷和定期备份；MVP 只使用内存 session，重启后要求客户端重新加入，任何 profile 的数据面都不在每个包路径查询数据库。

服务器上的网络关系：

```text
公网客户端
   │  WireGuard UDP
   ▼
wg0: 10.240.0.1/24
   │
   └── Civ6 relay → 当前房主的 10.10.0.X
```

## 2. Rust MVP service

MVP 运行 Rust 控制面和 UDP relay；Python 文件只保留为早期 loopback
prototype，不应被 systemd 作为生产入口启动：

```bash
git clone <repository> /opt/civ6-lan-bridge
cd /opt/civ6-lan-bridge
cargo build --release -p civ6-lan-server
sudo useradd --system --home-dir /var/lib/civ6-lan-bridge \
  --shell /usr/sbin/nologin civ6-relay || true
sudo install -d -o civ6-relay -g civ6-relay /var/lib/civ6-lan-bridge
sudo install -o root -g root -m 0755 \
  target/release/civ6-lan-server /usr/local/bin/civ6-lan-server
sudo cp systemd/civ6-relay.service /etc/systemd/system/
sudo cp config/civ6-relay.example.env /etc/civ6-lan-bridge.env
sudo chmod 600 /etc/civ6-lan-bridge.env
sudoedit /etc/civ6-lan-bridge.env
```

至少设置：

```env
CIV6_CONTROL_BIND=127.0.0.1:8080
CIV6_CONTROL_BEARER_TOKEN=<至少 32 个随机字符>
CIV6_RELAY_BIND=10.240.0.1:32000
CIV6_WIREGUARD_INTERFACE=wg0
```

启动并检查：

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now civ6-relay
sudo systemctl status civ6-relay
sudo journalctl -u civ6-relay -f
sudo tcpdump -ni wg0 'udp portrange 62900-62999 or udp port 62056'
```

防火墙只允许 WireGuard peer 访问 relay envelope 端口：

```bash
sudo nft add rule inet filter input iifname "wg0" udp dport 32000 accept
```

真实环境应把规则写入持久化 nftables 配置，并先确认现有防火墙链不会重复添加规则。

## 3. Production control plane

正式客户端不能让用户手工分配 `10.10.0.X`。服务端需要提供以下控制 API：

```text
POST /v1/rooms                 创建房间，返回一次性 room code
POST /v1/rooms/{code}/join    注册客户端 public key，返回 tunnel 配置
POST /v1/rooms/{code}/hosts   注册一个短期 Civ6 host session
POST /v1/rooms/{code}/heartbeat
DELETE /v1/rooms/{code}/hosts/{host_session_id}
DELETE /v1/rooms/{code}/peers/{id}
GET  /v1/rooms/{code}/status
```

数据库至少保存：`room_id`、短期 token 哈希、peer public key、虚拟 IP、角色、host session、最后心跳、创建时间和过期时间。私钥永远只在客户端生成，服务端不保存。

一个 room 可以同时存在多个 host session，不能把房主写死成单一环境变量。relay 的路由键应是：

```text
(room_id, discovery_request_id, host_peer_id) → client_virtual_ip
(room_id, gameplay_session_id) → host_virtual_ip
```

这样一个房间内任意成员都可以创建房间，其他成员能看到并选择对应房主；不同 room code 之间仍然隔离。

Rust 控制面和 UDP relay 当前可用环境变量：

    CIV6_CONTROL_BIND=127.0.0.1:8080
    CIV6_RELAY_BIND=10.240.0.1:32000
    CIV6_RELAY_PORT=32000
    CIV6_CONTROL_BEARER_TOKEN=<至少 32 个随机字符>
    CIV6_DATABASE_URL=postgres://...
    CIV6_WIREGUARD_INTERFACE=wg0

数据库 migration 已保存于 `server/migrations/0001_control_plane.sql`。设置数据库后，房间、peer、host session 和 gameplay session 的 mutation 会写入 PostgreSQL；启动时恢复房间和 peer，清理旧 host/gameplay session，并重新向 WireGuard 接口写入已登记 peer。数据面每个包仍只查内存路由，不访问 PostgreSQL。

`CIV6_RELAY_BIND` 绑定的是 WireGuard 虚拟接口地址，不应绑定公网网卡。它承载服务端和桌面适配器之间的版本化 relay envelope；客户端适配器负责将 envelope 还原为本机 Civ VI 的 discovery 或 gameplay UDP 包。

服务端启动时会记录 control endpoint、relay endpoint、relay 端口、协议版本、构建 commit 和 PID。受 Bearer token 保护的 `/v1/test/metrics` 返回 relay 收发、丢弃、重复、字节数、活动 peer/room 和认证失败计数。macOS transport-level 验收合同见 [`docs/mac-e2e-test.md`](mac-e2e-test.md)；它不会把 synthetic fan-out 误报为 Civ VI 房间发现。

## 4. Client release pipeline

客户端构建不在服务端执行：

1. CI 从同一个 commit 检出 `server/`、`win-client/`、`mac-client/`；
2. Windows runner 构建并签名 EXE、WFP/兼容网络组件和安装器；
3. macOS runner 构建 Intel/Apple Silicon，签名 Network Extension 和 App，生成 DMG；
4. macOS 使用 Developer ID 和 `notarytool` 公证，随后 staple ticket；
5. 发布 SHA-256、版本号和 Git commit；
6. 服务端只提供版本清单和签名发布文件，不直接让客户端下载任意二进制。

## 5. Acceptance test

每个版本至少验证：

- Windows 10/11 x64；
- macOS Intel 和 Apple Silicon；
- 家庭宽带、手机热点、校园/公司网络各一组；
- 创建房间、刷新发现、加入房间、连续 30 回合；
- 一方切换网络后的自动重连；
- relay 重启后的客户端恢复；
- 两个不同 room code 之间互相不可见；
- 未授权公网 UDP 包不会被 relay 转发。

正式实现还必须补充：

- 任意成员连续创建两个 host session，其他成员能看到并准确加入对应房间；
- 两个 room code 同时在线时，discovery 和 `62056/UDP` 不串线；
- relay 崩溃/重启后旧 gameplay session 失效，客户端可以自动重新注册；
- WireGuard peer public key、虚拟 IP 和 room token 不匹配时请求被拒绝；
- Windows WFP/兼容适配器和 macOS Network Extension 都只拦截/路由 Civ VI 相关流量。
