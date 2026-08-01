# Civ6 LAN Bridge monorepo architecture

## Repository layout

```text
civ6-lan-bridge/
├── server/       # Rust control plane + WireGuard peer manager + UDP relay
├── win-client/   # Windows 10+ Tauri app + WFP outbound adapter
├── mac-client/   # macOS Tauri app + Swift Network Extension
├── clients/ui/    # shared TypeScript UI package and styles
├── crates/       # shared Rust protocol and packet parsing crates
├── docs/         # protocol, deployment and release notes
├── config/       # example configuration only; no private keys
├── scripts/      # server diagnostics and packet capture
└── tests/        # relay and protocol tests
```

这三个目录就是三个可独立构建的项目，同时由根目录 Git 仓库统一管理版本、协议和发布号。`clients/ui` 是两个桌面项目共同依赖的前端包；两个 Tauri 壳只保留平台入口、原生命令和各自的打包配置。

## Runtime topology

```text
Windows Civ6 ─┐
              ├─ platform client ─ encrypted tunnel ─┐
macOS Civ6 ───┘                                      │
                                                   server
                                      control API + UDP relay + WireGuard
```

服务端不解析 Civ VI 房间 JSON，只负责认证、会话绑定、发现 fan-out 和 UDP 转发。这样可以避免依赖游戏数据格式，也能让多个房间使用独立的 host/session 映射。

正式版本的默认安全传输是 WireGuard L3 tunnel；relay 从 Phase 1 起建立在可替换 `DatagramTransport` 之上，`udp2raw`/QUIC 只能作为实测 fallback。Windows 生产适配层必须在 WFP outbound transport layer 截获 Civ6 广播并改写目标，不能靠被动监听虚拟网卡。macOS 使用 `NEPacketTunnelProvider`，只把 Civ VI 相关虚拟路由送入远端 tunnel。

## Room lifecycle

1. 用户在客户端创建或加入房间，客户端生成本机密钥；
2. 客户端用一次性 room token 调用服务端控制 API；
3. 服务端分配虚拟 IP、注册 WireGuard peer，并返回短期 tunnel 配置；
4. 任意成员创建 Civ VI 房间后，客户端向服务端注册一个短期 `host_session`；
5. 其他客户端刷新房间时，平台适配层将 Civ VI 广播转成 relay 请求；
6. 服务端把发现请求 fan-out 到该 room 的所有有效 `host_session`；
7. 每个房主的回复都保留 `host_peer_id`，客户端按房主分别展示房间；
8. 加入后，`62056/UDP` 根据选中的 `host_peer_id` 路由到对应房主。

## Security rules

- 房间 token 只允许加入指定房间，不能直接代表服务端管理员权限；
- WireGuard 私钥只在客户端生成和保存，服务端只保存 public key；
- relay 必须校验来源 peer、room/session 和允许的端口；
- 不允许任意公网地址作为 relay 上游，防止 UDP reflector；
- 控制 API 使用 HTTPS、短期 token、速率限制和结构化审计日志；
- 客户端断开或 token 过期后，服务端删除 peer 和 relay session。
- relay envelope 本身不重复实现一层加密或认证：数据面只绑定 WireGuard 接口，WireGuard peer 公钥负责传输层身份，relay 再校验虚拟 IP、room 和 session。relay 绝不能绑定公网网卡；若未来启用不经过 WireGuard 的 transport，必须增加 per-peer AEAD/authentication，而不是沿用当前 envelope 明文信任模型；
- Tauri 使用显式 CSP，禁止任意脚本、对象和表单来源；控制 API 请求由 Rust core 发出，不由 WebView 直接跨域访问。

客户端平台适配层还必须把 relay 返回的房主虚拟地址交给 Civ VI；如果只把回包统一伪装成 relay 地址，多个房主会在加入阶段串线。

## Stability requirements

- WireGuard `PersistentKeepalive=25` 处理移动网络和 NAT；
- relay 使用独立 session、空闲回收和上限保护；
- server systemd `Restart=on-failure`，并保留 journald 日志；
- 客户端显示四个独立状态：控制 API、隧道握手、relay、Civ VI 发现；
- 客户端还要显示 2K Online Services/年龄验证外部前置状态；
- 每次发布都要做两台真实设备的发现、加入、连续 30 回合 UDP 联机和断网重连测试；macOS Aspyr 跨平台组合的测试权重最高；
- Windows 安装器自动创建 Domain/Private 防火墙规则，并在卸载时清理；
- 版本、Rust toolchain、Tauri、WFP/驱动、Xcode 和构建依赖全部锁定。
- relay 的数据面不对每个 UDP 包查数据库；运行时 session 状态有界，控制面状态写入 PostgreSQL；
- 一个 room 使用单活 relay 亲和性，不能把同一 room 的 UDP 包随机分发到多个无状态节点；
- relay 重启不恢复旧 gameplay session，客户端重新鉴权、注册和建立 session；
- 生产发布前必须验证 malformed packet、UDP 放大、过期 token 和跨房间隔离。

## Build and release

Tauri 可以生成 macOS `.app/.dmg` 和 Windows NSIS `.exe/.msi`。建议使用 Windows/macOS CI runner 构建，不在服务端编译桌面客户端：

- Windows runner：构建 x64，签名 EXE、DLL、驱动和安装器；当前 CI 先产出未签名候选 NSIS 包，并校验安装器资源和防火墙 hook；
- Apple Silicon macOS runner：构建 ARM64，先构建未签名 Packet Tunnel `.appex`，通过 Tauri macOS `files` 配置嵌入 App 的 `Contents/PlugIns`，再在生产凭据可用时签名 Network Extension 和 App，生成 DMG，使用 `notarytool` 公证并 staple ticket；当前 provider 数据转发仍是 stub；
- 发布前上传 SHA-256、版本号和构建 commit，客户端只从受信发布地址更新。
