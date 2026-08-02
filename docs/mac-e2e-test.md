# macOS 端到端测试合同

这个测试合同验证的是“macOS 客户端能否通过认证控制面和 relay 数据面交换协议包”。它不把 UDP `255.255.255.255` 直接发送到公网：客户端发的是共享 `RelayMessage` envelope，服务端根据已登记的虚拟源地址识别 peer，再把逻辑广播转换为同一 room 内的单播 fan-out。

没有第二台真实运行 Civ VI 的 Windows/macOS 客户端时，`server-test-report.json` 的 `status` 最多是 `partial`，并且 `civ6_discovery` 必须是 `not_tested`。synthetic fan-out 通过不能证明 Civ VI UI 显示了房间。

## 本地可重复测试

在仓库根目录运行：

```bash
scripts/mac-e2e-server-test.sh
```

脚本会：

1. 生成只存在于 `/tmp` 的随机 Bearer token；
2. 以现有 `civ6-lan-server` 二进制启动控制 API 和 UDP relay；
3. 使用 `CIV6_VIRTUAL_IP_PREFIX=127.0.0` 为五个 synthetic peer 分配 loopback 地址。这只用于本地测试，不能用于生产 WireGuard；
4. 通过普通控制 API 创建两个专用 room、加入 peer、登记 host 和创建 gameplay session；
5. 通过真实 UDP socket 使用共享协议验证握手、echo、同房间 fan-out、双向 gameplay、跨房间隔离、Bearer 认证、源地址伪造、超大包、重复包、顺序、host TTL 和 relay 断开错误；
6. 写出 `server-test-report.json`，并把服务端日志和脱敏 manifest 放在临时测试目录；
7. 停止服务，销毁原始 manifest 和 token 文件。保留的脱敏 manifest 中 token 为 `<destroyed>`。

可覆盖的本地配置：

```bash
CIV6_TEST_CONTROL_PORT=18080 \
CIV6_TEST_RELAY_PORT=32000 \
CIV6_TEST_REPORT=/tmp/server-test-report.json \
scripts/mac-e2e-server-test.sh
```

服务端启动日志会明确包含 `control_endpoint`、`relay_endpoint`、`relay_port`、`protocol_version`、`build_commit` 和 `pid`。测试过程中的真实 token 不打印到终端或日志。

## Mac 端读取 manifest

测试 runner 生成的原始 manifest 只在测试运行期间有效。Mac 端可通过环境变量指定路径：

```bash
export CIV6_TEST_MANIFEST=/tmp/civ6-lan-bridge-mac-e2e.XXXXXX/session-manifest.json
```

在 macOS Tauri 客户端中点击“读取 macOS 测试 manifest”会调用 `load_test_manifest`，填入控制 API、Bearer token、relay 地址、room code、peer ID 和本机虚拟地址。该命令只接受本地文件，不会把 token 写入 Git；测试完成后应删除原始 manifest。当前仓库的 Packet Tunnel provider 仍有真实 WireGuard send/inject TODO，因此该按钮和 relay probe 是诊断入口，不是 Civ VI 已联机的承诺。协议冒烟测试同时覆盖 legacy v1 解码与带 `sequence`、`connection_epoch`、`sent_at_ms`、`path_id` 的 v2 envelope。

如果 Mac 代理要把结果交回 server runner，可设置：

```bash
export CIV6_TEST_MAC_RESULT=/tmp/civ6-mac-result.json
export CIV6_TEST_WAIT_FOR_MAC_SECONDS=900
```

结果文件最小格式：

```json
{
  "session_id": "runner-or-mac-session-id",
  "client_id": "mac-peer-id",
  "civ6_discovery": "pass",
  "evidence_files": ["/tmp/mac-packet-tunnel.log"]
}
```

runner 会等待该文件出现；未提供时不阻塞本地 synthetic 测试，并报告 `mac_client_id: null`、`civ6_discovery: not_tested`。

## 生产/云服务器运行合同

生产环境不要使用本地脚本的 loopback 地址。Rust 服务仍使用同一启动方式，示例：

```env
CIV6_CONTROL_BIND=127.0.0.1:8080
CIV6_RELAY_BIND=10.240.0.1:32000
CIV6_RELAY_PORT=32000
CIV6_CONTROL_BEARER_TOKEN=<至少 32 个随机字符>
CIV6_WIREGUARD_INTERFACE=wg0
CIV6_BUILD_COMMIT=<部署的 Git commit>
```

网络合同：

- TCP `443`：只给 Caddy/反向代理的 HTTPS 控制 API；本地 Rust 控制 API 不直接暴露公网。
- UDP WireGuard 端口（例如 `51820/UDP`）：只允许客户端加入 WireGuard 的入口。
- UDP `32000`：只绑定 `wg0` 的 `10.240.0.1`，只允许 WireGuard peer/安全组访问 relay envelope；不要对公网开放任意 UDP 转发。
- 不需要、也不允许配置公网 `255.255.255.255` 广播规则。云安全组和主机 nftables 都应放行实际 WireGuard/relay 端口，而不是 Civ VI 广播地址。
- relay envelope 通过 WireGuard 的 L3 peer 身份认证；控制 API 使用 Bearer token，并且生产应通过 HTTPS 传输。没有 WireGuard peer 注册，服务端会丢弃来源虚拟地址未知的 UDP 包。

部署后可观察：

```bash
curl -fsS https://civ6.example.com/health/live
curl -fsS -H "Authorization: Bearer $CIV6_CONTROL_BEARER_TOKEN" \
  https://civ6.example.com/v1/test/metrics
sudo journalctl -u civ6-relay -f
```

`/v1/test/metrics` 是受 Bearer 保护的服务端观测端点，返回 `sent_packets`、`received_packets`、`dropped_packets`、`duplicated_packets`、`reordered_packets`、`bytes_in`、`bytes_out`、`active_peers`、`active_rooms`、`active_hosts` 和 `authentication_failures`。协议 v1 没有 envelope 序列号，所以 `reordered_packets` 由 runner 使用不透明测试 payload 的顺序观测得出，不宣称公网 UDP 没有乱序。

## 判定标准

- `pass`：服务端传输检查全部通过，并且有真实 Mac/Windows Civ VI 第二客户端证据把 `civ6_discovery` 标为 `pass`。
- `partial`：控制面、认证、UDP relay、同房间 fan-out 和隔离全部通过，但没有真实 Civ VI 第二客户端；这是当前纯 server/synthetic runner 的正常结果。
- `fail`：任一认证、握手、UDP、fan-out、隔离、TTL、MTU 或断开错误检查失败。此时不能把 DMG 说成网络可用。

报告中的 `evidence_files` 应能定位到服务端启动日志、脱敏 manifest，以及可选的 Mac Packet Tunnel/Console 日志。真实 Civ6 房间发现仍需要第二个真实客户端和实际 Packet Tunnel 数据面运行记录。
