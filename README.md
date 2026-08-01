# civ6-lan-bridge

通过现有 WireGuard 中继、服务端 UDP relay 和 Civ VI 的 IP 注入式发现，解决跨网络玩家看不到 LAN 房间的问题。

本仓库现在按一个大 Git 仓库管理三个项目：

- `server/`：Rust 房间控制面、PostgreSQL 恢复、WireGuard peer 管理和 UDP envelope relay；
- `win-client/`：Windows 10+ 一键客户端；
- `mac-client/`：macOS 一键客户端。
- `clients/ui/`：Windows/macOS 共用的 TypeScript UI、样式和前端配置。

完整工程规格见 [`docs/spec.md`](docs/spec.md)，协议基线见 [`docs/protocol.md`](docs/protocol.md)，架构和部署细节见 [`docs/architecture.md`](docs/architecture.md) 与 [`docs/deployment.md`](docs/deployment.md)。Rust 服务端核心、共享 datagram client core、共享桌面 UI、Tauri 诊断客户端和跨平台 CI 已建立；Windows WFP、macOS Packet Tunnel 的真实注入、签名和实机 Civ6 验收仍是发布门槛。

## 项目目标

这个项目不改动 Civ VI 的游戏数据，也不在服务器上伪造房间。它只处理网络发现路径：

1. 玩家通过 WireGuard 加入同一个三层私网；
2. WireGuard 作为默认的加密 L3 传输；relay 从 Phase 1 就依赖可替换的 datagram transport，`udp2raw`/QUIC 只能在实网验证必要时作为 fallback；
3. 客户端适配器将 Civ VI 发往 `255.255.255.255` 的发现广播封装为 relay 请求；
4. 服务端按 room、host session 和 gameplay session 转发发现包与游戏包；
5. 游戏本身继续使用 Civ VI 原生的 UDP 联机协议。

## 现有服务器拓扑

本项目复用服务器上已有的配置，不创建第二个 WireGuard 实例：

```text
玩家 A / Windows Civ VI
  └─ injciv6 -> 房主的 WG IP: 10.10.0.X
       └─ WireGuard wg0: 10.10.0.0/24
            └─ Ubuntu 中继: <server-public-endpoint> / 10.10.0.1
                 └─ 房主 / Windows Civ VI
```

服务器当前已发现：

- `wg0`：`10.10.0.1/24`
- `udp2raw`：`54321`，转发到本地 WireGuard `51820`
- Civ VI 发现端口：`62900-62999/UDP`
- Civ VI 联机端口：`62056/UDP`
- IPv4 forwarding 已开启，`wg0 <-> wg0` 转发规则已存在

当前配置中的示例 peer：

- Mac：`10.10.0.10`
- Windows：`10.10.0.11`
- 其他 peer：`10.10.0.2`、`10.10.0.3`、`10.10.0.12-14`

不要把服务器公网地址或 `10.10.0.1` 填成 Civ VI 房主地址；客户端注入目标应当是“实际开房玩家”的 `10.10.0.X`。

## 为什么会看不到房间

这里的“255”不是端口，而是 `255.255.255.255`，即 IPv4 limited broadcast 地址。文明六点击刷新时，会向 `62900-62999/UDP` 发广播包；房主收到后，再向请求方单播返回房间 JSON。进入房间后，游戏会改用 `62056/UDP`。

WireGuard、普通 VPN 和大多数异地组网工具是三层网络，只转发已知的单播 IP，不会自动传递二层广播。因此“WireGuard 能 ping 通，但 LAN 列表为空”并不代表 WireGuard 故障。服务端本身也无法凭空看到玩家物理网卡上的广播，客户端仍必须使用 `injciv6` 或等价的本地代理把广播目标改成服务端 IP。

## 旧版网络验证：Python relay

仓库提供了不依赖第三方 Python 包的服务端 relay：[`server/civ6-relay.py`](server/civ6-relay.py)。它只绑定服务器的 WireGuard IP，不监听公网；每个客户端有独立的上游 UDP socket，因此多个客户端同时加入时不会把 `62056` 回包混在一起。

服务端安装：

```bash
sudo install -d /opt/civ6-lan-bridge
sudo cp -a server /opt/civ6-lan-bridge/
sudo cp systemd/civ6-relay.service /etc/systemd/system/
sudo cp config/civ6-relay.example.env /etc/civ6-relay.env
sudoedit /etc/civ6-relay.env
sudo systemctl daemon-reload
sudo systemctl enable --now civ6-relay
sudo systemctl status civ6-relay
```

`CIV6_HOST_WG_IP` 填当前开房玩家的 WireGuard 地址；`CIV6_RELAY_LISTEN_IP` 填服务器的 WireGuard 地址 `10.10.0.1`。防火墙只需要放行 `wg0` 来源的 `62900-62999/UDP` 和 `62056/UDP`，不要把这些端口暴露到公网网卡。

Windows 客户端把发现目标改成服务端 WG IP，而不是房主 IP：

```powershell
.\clients\windows\prepare-injciv6.ps1 `
  -Civ6Directory 'C:\Program Files (x86)\Steam\steamapps\common\Sid Meier''s Civilization VI\Base\Binaries\Win64Steam' `
  -RelayIp '10.10.0.1'
```

先启动 WireGuard，再启动 Civ VI，最后运行 `injciv6`（同一个游戏进程只注入一次）。房间列表里显示的目标会是 relay 的 WG 地址；点击加入后，`62056/UDP` 也由同一个 relay 转发到房主。

这个旧版验证程序按“一个 relay 对应一个当前房主”设计。更换房主时只需修改 `/etc/civ6-relay.env` 中的 `CIV6_HOST_WG_IP` 并重启服务。正式多房间实现使用 Rust 服务端和 `CIV6_RELAY_BIND`，不再依赖这个手工配置。

```bash
sudo systemctl restart civ6-relay
```

## 端口与协议

根据 Civ VI 的发现流程，客户端会在 `62900-62999/UDP` 范围内发送发现请求，房主通常在 `62900/UDP` 监听，实际联机使用 `62056/UDP`。因此：

- 所有参与者必须能互相访问 WireGuard peer IP；
- 服务器必须允许 `wg0 -> wg0` 的转发；
- 服务器和客户端防火墙都不能拦截 `62056/UDP` 与 `62900-62999/UDP`；
- 使用旧的直连模式时，注入目标是房主的 WireGuard peer IP；使用本仓库 relay 模式时，注入目标改为服务端的 WireGuard IP。

## 当前桌面客户端构建状态

`win-client/` 和 `mac-client/` 使用各自的 Tauri 原生壳，但 UI 入口、样式和 TypeScript 编译配置来自 `clients/ui/`。因此 UI 修改只维护一份；平台差异只能进入 Tauri Rust 命令、Windows WFP 或 macOS Network Extension。

本仓库的 `.github/workflows/desktop-build.yml` 在 Windows runner 生成 NSIS `.exe`，在 macOS Intel/Apple Silicon runner 生成 DMG，并额外构建未签名 Packet Tunnel `.appex` 作为 macOS 扩展校验制品。当前 Linux 环境不能生成真实 Windows/macOS 制品；在 WFP/Packet Tunnel 数据转发和签名完成前，这些 CI 制品仍是候选包，不代表 Civ VI 端到端联机已验收。

## Windows 客户端流程（旧版手工验证）

1. 先启动 WireGuard，确认能 ping 通房主的 `10.10.0.X`；
2. 启动 Civ VI，停留在主菜单；
3. 运行 `clients/windows/prepare-injciv6.ps1`，把服务端 relay 地址写入游戏目录的 `injciv6-config.txt`；
4. 以管理员身份运行你已取得的 `injciv6.exe` 或 `injciv6-gui.exe`，只注入一次；
5. 回到 Civ VI 的 LAN 房间列表刷新。

项目不自动下载或执行第三方注入器。注入器属于客户端软件，必须由每位玩家自行确认来源、版本和杀毒软件策略。

## 预检

在 Ubuntu 服务器上运行：

```bash
sudo ./scripts/preflight-server.sh
```

在客户端和房主之间先验证：

```bash
ping 10.10.0.X
```

服务器侧抓包验证发现流量是否到达：

```bash
sudo ./scripts/capture-civ6.sh 10.10.0.X
```

停止抓包用 `Ctrl-C`。脚本只读，不会改 WireGuard 或防火墙。

## 已知边界

- WireGuard 是三层隧道，不会自动转发二层广播；所以只连上 WireGuard、但不做 IP 注入或本地代理时，Civ VI 仍可能看不到房间。
- 服务端 relay 不能代替客户端的广播改写；如果客户端完全不能安装/运行 `injciv6` 或本地代理，就需要改用支持二层广播的 TAP/桥接型 VPN，这不是当前 WireGuard 配置本身能解决的。
- 旧版 Python relay 明确配置一个房主；Rust relay 使用动态 host session，但客户端平台适配器尚未完成。
- `injciv6` 的原始项目主要面向 Windows；Mac/Linux 客户端需要另行采用广播转发或手动指定房主地址的方案。
- 服务器上的 `wg-quick@wg0` 当前需要单独修复 systemd 状态；本项目暂不重启它，避免影响现有 peer。
- 房主必须先能连接 2K Online Services 完成账号/年龄验证；该外部依赖失败时，Civ VI 可能在“确认设置”阶段不创建 LAN 房间，本服务不绕过 2K 检查。

## 验证

运行单元测试：

```bash
make relay-test
```

服务端看 relay 日志：

```bash
sudo journalctl -u civ6-relay -f
```

如果房间仍为空，先确认服务端看到来自客户端的 `62900-62999/UDP`，再确认房主收到并回包：

```bash
sudo tcpdump -ni wg0 'udp portrange 62900-62999 or udp port 62056'
```
