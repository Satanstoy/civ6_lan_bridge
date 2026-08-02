# macOS client

目标系统：macOS 12 及以上，当前发布 Apple Silicon（ARM64）架构。

macOS 端不能照搬 Windows DLL 注入，也不应该依赖旧的 TAP kernel extension。采用原生 Network Extension：

- Tauri 负责 UI、房间码和客户端状态；
- Swift `NEPacketTunnelProvider` 负责创建系统认可的虚拟 IP 接口；
- Packet Tunnel Provider 只接管 Civ VI 所需的目的地址/UDP 流量，并封装到服务端 relay；
- 游戏收到的房间和后续 `62056/UDP` 都走同一条会话。

`NEPacketTunnelProvider` 的生命周期和 entitlement 示例位于 [`PacketTunnel/`](PacketTunnel/)。它还需要把 IPv4/UDP 解析和共享 Rust relay transport 嵌入最终 Xcode target；当前目录中的 Tauri probe 不能冒充完整 Civ VI 注入实现。

`NEPacketTunnelProvider` 在 macOS Developer ID 直接分发时必须作为 System Extension 打包，并使用 `packet-tunnel-provider-systemextension` entitlement。发布到用户机器时还需要 Developer ID 签名、Hardened Runtime、notarization 和 staple。DMG 只负责分发，真正的网络扩展也必须一起签名。主分支/PR 仍可生成未签名验证 artifact；`v*` tag 会强制要求签名 secrets、两个 provisioning profile，并在成功 notarization 后才进入 release job。

开发构建需要 macOS + Xcode；Linux 机器不能验证 Network Extension，也不能生成可直接通过 Gatekeeper 的最终 DMG。GitHub Actions 会在 Apple Silicon runner 构建 ARM64 DMG 候选，发布前仍必须在真实设备上测试 WireGuard 路由、Civ VI 本地网络权限、2K 年龄验证和房间发现；Aspyr 跨平台联机测试权重最高。
