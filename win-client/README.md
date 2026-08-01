# Windows client

目标系统：Windows 10 及以上，优先支持 x64。

Windows 端采用 Tauri + Rust 核心，正式网络适配按 Windows Filtering Platform (WFP) 设计：

- 将 Civ VI 发往 `255.255.255.255:62900-62999` 的发现包改为发往服务端 relay；
- 让 relay 返回的房间信息回到 Civ VI；
- 将加入房间后的 `62056/UDP` 保持在同一个 relay 会话；
- 不修改游戏文件，不需要用户手动运行 `injciv6`。

WFP 是 Windows 的网络过滤和修改平台。现有 WinDivert 适配只作为原型/兼容路径保留；如果在第一阶段交付 WinDivert，生产包必须固定版本、验证来源和签名，并对 EXE、DLL/驱动和安装器一起做 Authenticode 签名。正式版仍需完成 WFP 驱动的安装、更新、卸载、回滚和兼容性测试。

现有的 [`clients/windows/prepare-injciv6.ps1`](../clients/windows/prepare-injciv6.ps1) 仅作为兼容旧方案保留；一键客户端完成后不再要求用户调用它。

当前可验证内容：Tauri UI、Rust client core、共享 relay envelope 和 UDP probe 已可在 Linux 上完成 Rust 检查；Windows `.exe` 的权威构建在 GitHub Actions `windows-latest` 上执行。WFP callout/driver 仍是下一阶段的原生 Windows 适配器，具体契约见 [`wfp/README.md`](wfp/README.md)。

安装器必须自动执行 `install.ps1` 或等价逻辑，给 bridge relay 和已识别的 Civ6 程序创建 Domain/Private 防火墙规则，卸载时删除规则。这样“能 ping 通但搜不到”的本机防火墙问题不会依赖用户手工排查。

开发构建建议：

```powershell
npm install
npm run tauri build
```

发布包应使用 NSIS 安装器，并在安装/卸载时正确注册和移除网络适配器组件。客户端必须显示管理员权限、驱动状态、WireGuard 握手状态、relay 连通状态，以及 2K Online Services/年龄验证是外部前置条件。
