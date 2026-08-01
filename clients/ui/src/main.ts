import "./style.css";

export type ClientPlatform = "windows" | "macos";
export type InvokeFunction = <T>(
  command: string,
  args?: Record<string, unknown>,
) => Promise<T>;

export type BridgeUiOptions = {
  platform: ClientPlatform;
  invoke: InvokeFunction;
};

type Settings = {
  control_url: string;
  bearer_token: string;
  relay_server: string;
  relay_port: number;
};

function render({ platform, invoke }: BridgeUiOptions): void {
  const app = document.getElementById("app");
  if (!app) {
    throw new Error("shared UI mount point #app is missing");
  }

  const platformName = platform === "windows" ? "Windows 10+" : "macOS 12+";
  app.innerHTML = `
    <main>
      <h1>Civ6 LAN Bridge</h1>
      <p>${platformName} 客户端诊断与 relay 连接</p>
      <label>控制 API <input id="control-url" value="http://127.0.0.1:8080" /></label>
      <label>Bearer Token <input id="token" type="password" /></label>
      <label>Relay 地址 <input id="relay-server" value="10.240.0.1:32000" /></label>
      <label>本机虚拟地址 <input id="local-bind" value="10.240.0.2:32000" /></label>
      <h2>房间控制</h2>
      <label>房间码 <input id="room-code" placeholder="例如 ABC234" /></label>
      <label>本机 Peer ID <input id="peer-id" placeholder="加入房间后自动填入" /></label>
      <label>目标 Host Session ID <input id="host-session-id" placeholder="房主登记后填入" /></label>
      <div class="actions">
        <button id="health">检查控制 API</button>
        <button id="probe">检查 UDP Relay</button>
        <button id="create-room">创建房间</button>
        <button id="join-room">加入房间</button>
        <button id="register-host">登记为房主</button>
        <button id="create-gameplay">建立游戏路由</button>
        ${platform === "macos" ? '<button id="load-test-manifest">读取 macOS 测试 manifest</button>' : ""}
      </div>
      <pre id="status">未连接</pre>
    </main>
  `;

  const value = (id: string): string => {
    const element = document.getElementById(id);
    if (!(element instanceof HTMLInputElement)) {
      throw new Error(`shared UI input #${id} is missing`);
    }
    return element.value.trim();
  };

  const status = document.getElementById("status");
  if (!(status instanceof HTMLPreElement)) {
    throw new Error("shared UI status element is missing");
  }

  const settings = (): Settings => ({
    control_url: value("control-url"),
    bearer_token: value("token"),
    relay_server: value("relay-server"),
    relay_port: 32000,
  });

  const input = (id: string): HTMLInputElement => {
    const element = document.getElementById(id);
    if (!(element instanceof HTMLInputElement)) {
      throw new Error(`shared UI input #${id} is missing`);
    }
    return element;
  };

  const showResult = (result: unknown): void => {
    status.textContent = JSON.stringify(result, null, 2);
  };

  document.getElementById("health")?.addEventListener("click", async () => {
    status.textContent = "正在请求控制 API…";
    try {
      status.textContent = JSON.stringify(
        await invoke("health_live", { settings: settings() }),
        null,
        2,
      );
    } catch (error) {
      status.textContent = `控制 API 失败：${String(error)}`;
    }
  });

  document.getElementById("probe")?.addEventListener("click", async () => {
    status.textContent = "正在发送 relay probe…";
    try {
      const result = await invoke("relay_probe", {
        settings: settings(),
        localBind: value("local-bind"),
      });
      status.textContent = JSON.stringify(result, null, 2);
    } catch (error) {
      status.textContent = `UDP Relay 失败：${String(error)}`;
    }
  });

  document.getElementById("create-room")?.addEventListener("click", async () => {
    status.textContent = "正在创建房间…";
    try {
      const room = await invoke("create_room", { settings: settings() });
      const roomCode = String((room as { room_code: string }).room_code);
      input("room-code").value = roomCode;
      const peer = await invoke("join_room", {
        settings: settings(),
        roomCode,
      });
      input("peer-id").value = String((peer as { peer_id: string }).peer_id);
      input("local-bind").value = `${String(
        (peer as { virtual_ip: string }).virtual_ip,
      )}:32000`;
      showResult({ room, peer });
    } catch (error) {
      status.textContent = `创建并加入房间失败：${String(error)}`;
    }
  });

  document.getElementById("join-room")?.addEventListener("click", async () => {
    status.textContent = "正在加入房间…";
    try {
      const result = await invoke("join_room", {
        settings: settings(),
        roomCode: value("room-code"),
      });
      input("peer-id").value = String((result as { peer_id: string }).peer_id);
      input("local-bind").value = `${String(
        (result as { virtual_ip: string }).virtual_ip,
      )}:32000`;
      showResult(result);
    } catch (error) {
      status.textContent = `加入房间失败：${String(error)}`;
    }
  });

  document.getElementById("register-host")?.addEventListener("click", async () => {
    status.textContent = "正在登记 Civ VI 房主会话…";
    try {
      const result = await invoke("register_host", {
        settings: settings(),
        roomCode: value("room-code"),
        peerId: value("peer-id"),
      });
      input("host-session-id").value = String(
        (result as { host_session_id: string }).host_session_id,
      );
      showResult(result);
    } catch (error) {
      status.textContent = `登记房主失败：${String(error)}`;
    }
  });

  document.getElementById("create-gameplay")?.addEventListener("click", async () => {
    status.textContent = "正在建立游戏 UDP 路由…";
    try {
      const result = await invoke("create_gameplay_session", {
        settings: settings(),
        roomCode: value("room-code"),
        peerId: value("peer-id"),
        hostSessionId: value("host-session-id"),
      });
      showResult(result);
    } catch (error) {
      status.textContent = `建立游戏路由失败：${String(error)}`;
    }
  });

  document.getElementById("load-test-manifest")?.addEventListener("click", async () => {
    status.textContent = "正在读取 CIV6_TEST_MANIFEST…";
    try {
      const manifest = await invoke("load_test_manifest", {});
      const valueOf = (key: string): string => String((manifest as Record<string, unknown>)[key] ?? "");
      input("control-url").value = valueOf("control_endpoint");
      input("token").value = valueOf("token");
      input("relay-server").value = `${valueOf("relay_host")}:${valueOf("relay_port")}`;
      input("room-code").value = valueOf("room_code");
      input("peer-id").value = valueOf("client_id");
      input("local-bind").value = `${valueOf("client_virtual_ip")}:${valueOf("relay_port")}`;
      showResult({
        ...manifest as Record<string, unknown>,
        token: "<loaded; hidden after this view>",
      });
    } catch (error) {
      status.textContent = `读取测试 manifest 失败：${String(error)}`;
    }
  });
}

export function mountBridgeApp(options: BridgeUiOptions): void {
  render(options);
}
