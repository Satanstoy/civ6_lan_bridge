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

type RoomResponse = {
  room_code: string;
  member_count: number;
  host_count: number;
};

type PeerResponse = {
  peer_id: string;
  virtual_ip: string;
};

type AuthResponse = {
  username: string;
  access_token: string;
  expires_in_seconds: number;
};

type Room = {
  code: string;
  memberCount: number;
  isOwner: boolean;
  peerId?: string;
};

type View = "auth" | "lobby" | "room";
type Reachability = "checking" | "healthy" | "offline";

const RECENT_ROOMS_KEY = "civ6-lan-bridge.recent-rooms";
const USER_KEY = "civ6-lan-bridge.user";
const TOKEN_KEY = "civ6-lan-bridge.access-token";

function escapeHtml(value: string): string {
  return value.replace(
    /[&<>'"]/g,
    (character) =>
      ({
        "&": "&amp;",
        "<": "&lt;",
        ">": "&gt;",
        "'": "&#39;",
        '"': "&quot;",
      })[character] ?? character,
  );
}

function describeError(error: unknown): string {
  const message = String(error);
  if (/invalid relay address/i.test(message)) {
    return "中继服务器配置无效，请联系管理员检查服务器地址和端口。";
  }
  if (/invalid control endpoint/i.test(message)) {
    return "服务端地址配置无效，请联系管理员检查控制面 URL。";
  }
  if (/invalid relay port/i.test(message)) {
    return "中继端口配置无效，请联系管理员检查服务器端口。";
  }
  if (/401|unauthorized|valid bearer token/i.test(message)) {
    return "服务端鉴权未配置或已失效，请联系管理员更新客户端服务配置。";
  }
  if (/connection refused|error sending request|failed to connect|timed out/i.test(message)) {
    return "暂时无法连接服务，请检查网络后重试。";
  }
  return message;
}

function describeAuthError(error: unknown, mode: "register" | "login"): string {
  const message = String(error);
  if (/401|unauthorized|valid bearer token/i.test(message)) {
    return "用户名或密码不正确。";
  }
  if (/username is already registered|409|conflict/i.test(message)) {
    return "这个用户名已经被使用，请登录或更换用户名。";
  }
  if (/username must|password must|400|bad request/i.test(message)) {
    return mode === "register" ? "请检查用户名和密码格式。" : "用户名或密码格式不正确。";
  }
  return describeError(error);
}

function readRecentRooms(): string[] {
  try {
    const value = JSON.parse(localStorage.getItem(RECENT_ROOMS_KEY) ?? "[]");
    return Array.isArray(value)
      ? value.filter((room): room is string => typeof room === "string").slice(0, 4)
      : [];
  } catch {
    return [];
  }
}

function saveRecentRoom(code: string): void {
  const rooms = [code, ...readRecentRooms().filter((room) => room !== code)].slice(0, 4);
  localStorage.setItem(RECENT_ROOMS_KEY, JSON.stringify(rooms));
}

function settings(): Settings {
  return {
    control_url: "https://satanstoy.site/civ6-api",
    bearer_token: localStorage.getItem(TOKEN_KEY) ?? "",
    relay_server: "10.10.0.1:32000",
    relay_port: 32000,
  };
}

function icon(name: "arrow" | "back" | "copy" | "plus" | "signal" | "users"): string {
  const paths: Record<string, string> = {
    arrow: '<path d="M5 12h14M13 6l6 6-6 6"/>',
    back: '<path d="m15 18-6-6 6-6"/>',
    copy: '<rect x="9" y="9" width="10" height="10" rx="2"/><path d="M15 9V7a2 2 0 0 0-2-2H7a2 2 0 0 0-2 2v6a2 2 0 0 0 2 2h2"/>',
    plus: '<path d="M12 5v14M5 12h14"/>',
    signal: '<path d="M5 18h.01M9 15h.01M13 12h.01M17 9h.01"/><path d="M4 18a11 11 0 0 1 14-10M7 18a8 8 0 0 1 10-7M10 18a5 5 0 0 1 6-4"/>',
    users: '<path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M22 21v-2a4 4 0 0 0-3-3.87M16 3.13a4 4 0 0 1 0 7.75"/>',
  };
  return `<svg viewBox="0 0 24 24" aria-hidden="true">${paths[name]}</svg>`;
}

function render({ platform, invoke }: BridgeUiOptions): void {
  const app = document.getElementById("app");
  if (!app) throw new Error("shared UI mount point #app is missing");

  let view: View = localStorage.getItem(USER_KEY) && localStorage.getItem(TOKEN_KEY) ? "lobby" : "auth";
  let authMode: "register" | "login" = "register";
  let userName = localStorage.getItem(USER_KEY) ?? "";
  let currentRoom: Room | null = null;
  let roomPing: number | null = null;
  let roomReachability: Reachability = "checking";
  let roomPingTimer: number | undefined;
  let serviceReachability: Reachability = "checking";
  let serviceCheckInFlight = false;
  let notice: { type: "success" | "error"; text: string } | null = null;
  let busy = false;

  const setNotice = (type: "success" | "error", text: string): void => {
    notice = { type, text };
  };

  const inputValue = (id: string): string => {
    const element = document.getElementById(id);
    return element instanceof HTMLInputElement ? element.value.trim() : "";
  };

  const setBusy = (value: boolean): void => {
    busy = value;
    document.querySelectorAll<HTMLButtonElement>("button").forEach((button) => {
      button.disabled = value;
    });
  };

  const stopRoomPing = (): void => {
    if (roomPingTimer !== undefined) {
      window.clearInterval(roomPingTimer);
      roomPingTimer = undefined;
    }
  };

  const refreshServiceStatus = async (): Promise<void> => {
    if (serviceCheckInFlight || view === "auth") return;
    serviceCheckInFlight = true;
    try {
      await invoke("health_live", { settings: settings() });
      serviceReachability = "healthy";
    } catch {
      serviceReachability = "offline";
    } finally {
      serviceCheckInFlight = false;
      if (view === "lobby") show();
    }
  };

  const show = (): void => {
    const platformLabel = platform === "windows" ? "Windows" : "Mac";
    app.innerHTML = `
      <div class="app-frame">
        ${view === "auth" ? renderAuth(platformLabel) : renderShell(platformLabel)}
      </div>
    `;
    bindEvents();
  };

  const renderBrand = (): string => `
    <div class="brand-mark" aria-hidden="true">
      <span></span><span></span><span></span>
    </div>
    <div class="brand-copy">
      <strong>Civ6 LAN Bridge</strong>
      <small>Private game rooms</small>
    </div>
  `;

  const renderAuth = (platformLabel: string): string => `
    <main class="auth-layout">
      <div class="auth-decoration"><div class="glow glow-one"></div><div class="glow glow-two"></div></div>
      <section class="auth-card" aria-labelledby="auth-title">
        <div class="brand auth-brand">${renderBrand()}</div>
        <div class="auth-heading">
          <p class="eyebrow">${authMode === "register" ? "WELCOME" : "WELCOME BACK"}</p>
          <h1 id="auth-title">${authMode === "register" ? "创建你的账号" : "登录 Civ6 LAN Bridge"}</h1>
          <p>${authMode === "register" ? "用一个用户名开始创建或加入游戏房间。" : "登录后继续你的房间。"}</p>
        </div>
        <form id="auth-form" class="auth-form">
          <label for="auth-username">用户名</label>
          <input id="auth-username" autocomplete="username" maxlength="24" placeholder="例如：Alex" value="${escapeHtml(userName)}" />
          <label for="auth-password">密码</label>
          <input id="auth-password" type="password" autocomplete="${authMode === "register" ? "new-password" : "current-password"}" placeholder="至少 6 位" />
          <button class="button button-primary button-wide" type="submit" ${busy ? "disabled" : ""}>
            ${authMode === "register" ? "创建账号" : "登录"} ${icon("arrow")}
          </button>
        </form>
        ${notice ? `<div class="notice ${notice.type}">${escapeHtml(notice.text)}</div>` : ""}
        <button class="link-button" id="toggle-auth" type="button">
          ${authMode === "register" ? "已经有账号？登录" : "还没有账号？创建一个"}
        </button>
        <p class="platform-note">${platformLabel} 客户端 · 只显示房间和连接状态</p>
      </section>
    </main>
  `;

  const renderShell = (platformLabel: string): string => `
    <div class="client-layout">
      <header class="topbar">
        <div class="brand">${renderBrand()}</div>
        <div class="topbar-user">
          <span class="online-dot"></span>
          <span>${escapeHtml(userName)}</span>
          <button id="logout" class="icon-button" type="button" title="退出登录">↗</button>
        </div>
      </header>
      ${view === "room" && currentRoom ? renderRoom(platformLabel) : renderLobby(platformLabel)}
    </div>
  `;

  const renderLobby = (platformLabel: string): string => {
    const recentRooms = readRecentRooms();
    const serviceLabel = serviceReachability === "healthy" ? "服务在线" : serviceReachability === "offline" ? "服务不可用" : "检查服务";
    return `
      <main class="page-content">
        <section class="page-heading">
          <div>
            <p class="eyebrow">${platformLabel.toUpperCase()} / ROOMS</p>
            <h1>准备好开始了吗，${escapeHtml(userName)}？</h1>
            <p class="muted">创建一个房间，或者使用朋友分享的房间码加入。</p>
          </div>
          <div class="connection-state ${serviceReachability}"><span class="online-dot"></span>${serviceLabel}</div>
        </section>
        <section class="room-actions">
          <article class="action-card create-card">
            <div class="card-icon blue">${icon("plus")}</div>
            <div class="action-copy"><h2>创建房间</h2><p>成为这个房间的房主，邀请朋友加入。</p></div>
            <button class="button button-primary" id="create-room" type="button" ${busy ? "disabled" : ""}>创建房间 ${icon("arrow")}</button>
          </article>
          <article class="action-card join-card">
            <div class="card-icon violet">${icon("arrow")}</div>
            <div class="action-copy"><h2>加入房间</h2><p>输入朋友发来的 6 位房间码。</p></div>
            <div class="join-controls"><input id="room-code" maxlength="6" autocomplete="off" placeholder="ABC123" /><button class="button button-secondary" id="join-room" type="button" ${busy ? "disabled" : ""}>加入 ${icon("arrow")}</button></div>
          </article>
        </section>
        <section class="rooms-section">
          <div class="section-heading"><div><h2>最近的房间</h2><p>你最近使用过的房间会显示在这里。</p></div></div>
          ${recentRooms.length ? `<div class="room-list">${recentRooms.map((code) => `<button class="room-row" data-room-code="${escapeHtml(code)}" type="button"><span class="room-row-icon">${icon("users")}</span><span class="room-row-main"><strong>${escapeHtml(code)}</strong><small>点击重新加入</small></span><span class="room-row-arrow">${icon("arrow")}</span></button>`).join("")}</div>` : `<div class="empty-state"><div class="empty-icon">${icon("users")}</div><strong>还没有最近的房间</strong><span>创建或加入第一个房间吧。</span></div>`}
        </section>
        ${notice ? `<div class="notice ${notice.type}">${escapeHtml(notice.text)}</div>` : ""}
        <p class="footer-note">连接只用于 Civ6 游戏流量 · 不接管其他互联网流量</p>
      </main>
    `;
  };

  const renderRoom = (platformLabel: string): string => {
    if (!currentRoom) return "";
    const pingText = roomReachability === "checking" ? "检测中" : roomReachability === "offline" ? "不可用" : `${roomPing ?? 0} ms`;
    const roomStatus = roomReachability === "healthy" ? "房间已连接" : roomReachability === "offline" ? "连接异常" : "正在连接";
    const roomStatusHelp = roomReachability === "healthy" ? "服务器正在维持这条专用链路" : roomReachability === "offline" ? "正在等待网络恢复，客户端会继续尝试" : "正在探测中继服务器";
    return `
      <main class="page-content room-page">
        <button id="back-lobby" class="back-button" type="button">${icon("back")}返回房间列表</button>
        <section class="room-header">
          <div><p class="eyebrow">${platformLabel.toUpperCase()} / ROOM</p><h1>${escapeHtml(currentRoom.code)}</h1><p class="muted">把这个房间码分享给朋友即可加入。</p></div>
          <button id="copy-room" class="button button-quiet" type="button">${icon("copy")}复制房间码</button>
        </section>
        <section class="room-status-card">
          <div class="room-status-main"><span class="status-pulse ${roomReachability}"></span><div><strong>${roomStatus}</strong><span>${roomStatusHelp}</span></div></div>
          <div class="room-code-mini"><small>房间码</small><strong>${escapeHtml(currentRoom.code)}</strong></div>
        </section>
        <section class="room-grid">
          <article class="panel members-panel"><div class="panel-heading"><div><h2>房间成员</h2><p>${currentRoom.memberCount} 位成员</p></div><span class="member-count">${currentRoom.memberCount}</span></div><div class="member-list">
            ${currentRoom.isOwner ? `<div class="member-row"><span class="avatar owner">${escapeHtml(userName.slice(0, 1).toUpperCase())}</span><span class="member-details"><strong>${escapeHtml(userName)} <em>你</em></strong><small>房间房主</small></span><span class="host-badge">房主</span></div>` : `<div class="member-row"><span class="avatar">${escapeHtml(userName.slice(0, 1).toUpperCase())}</span><span class="member-details"><strong>${escapeHtml(userName)} <em>你</em></strong><small>已连接</small></span><span class="member-latency">—</span></div><div class="member-row waiting-row"><span class="avatar pending">?</span><span class="member-details"><strong>房间房主</strong><small>等待服务器同步</small></span><span class="member-latency">—</span></div>`}
          </div></article>
          <article class="panel latency-panel"><div class="panel-heading"><div><h2>连接质量</h2><p>你到中继服务器的延迟</p></div>${icon("signal")}</div><div class="latency-value"><strong>${pingText}</strong><span>Relay server</span></div><div class="latency-bar"><span style="width:${roomReachability === "offline" ? 8 : roomPing === null ? 35 : Math.min(95, Math.max(15, 100 - roomPing / 3))}%"></span></div><p class="latency-help">延迟只反映你到服务器的路径，不代表朋友之间的直连延迟。</p></article>
        </section>
        <div class="room-bottom"><span class="secure-label"><span class="lock-dot"></span>专用连接已启用</span><button id="leave-room" class="link-button danger" type="button">离开房间</button></div>
        ${notice ? `<div class="notice ${notice.type}">${escapeHtml(notice.text)}</div>` : ""}
      </main>
    `;
  };

  const refreshRoomPing = async (): Promise<void> => {
    if (!currentRoom) return;
    const roomCode = currentRoom.code;
    try {
      const started = performance.now();
      await invoke("relay_probe", { settings: settings(), localBind: "0.0.0.0:0" });
      if (!currentRoom || currentRoom.code !== roomCode) return;
      roomPing = Math.max(1, Math.round(performance.now() - started));
      roomReachability = "healthy";
    } catch {
      if (!currentRoom || currentRoom.code !== roomCode) return;
      roomPing = null;
      roomReachability = "offline";
    }
    show();
  };

  const joinRoom = async (code: string, owner: boolean): Promise<boolean> => {
    const normalized = code.toUpperCase().replace(/[^A-Z0-9]/g, "");
    if (normalized.length < 6) {
      setNotice("error", "请输入完整的 6 位房间码。");
      show();
      return false;
    }
    setBusy(true);
    show();
    try {
      const peer = await invoke<PeerResponse>("join_room", {
        settings: settings(),
        roomCode: normalized,
      });
      currentRoom = { code: normalized, memberCount: owner ? 1 : 2, isOwner: owner, peerId: peer.peer_id };
      saveRecentRoom(normalized);
      view = "room";
      roomPing = null;
      roomReachability = "checking";
      setNotice("success", "房间已准备好。");
      show();
      void refreshRoomPing();
      stopRoomPing();
      roomPingTimer = window.setInterval(() => void refreshRoomPing(), 2000);
      return true;
    } catch (error) {
      if (owner) {
        try {
          await invoke("delete_room", { settings: settings(), roomCode: normalized });
        } catch {
          // The room may already have been cleaned up by the server.
        }
      }
      setNotice("error", `加入房间失败：${describeError(error)}`);
      show();
      return false;
    } finally {
      setBusy(false);
      show();
    }
  };

  const bindEvents = (): void => {
    document.getElementById("toggle-auth")?.addEventListener("click", () => {
      authMode = authMode === "register" ? "login" : "register";
      notice = null;
      show();
    });
    document.getElementById("auth-form")?.addEventListener("submit", async (event) => {
      event.preventDefault();
      const name = inputValue("auth-username");
      const password = inputValue("auth-password");
      if (name.length < 2 || password.length < 6) {
        setNotice("error", "用户名至少 2 个字符，密码至少 6 位。");
        show();
        return;
      }
      setBusy(true);
      show();
      try {
        const auth = await invoke<AuthResponse>(authMode === "register" ? "register_user" : "login_user", {
          settings: settings(),
          username: name,
          password,
        });
        userName = auth.username;
        localStorage.setItem(USER_KEY, userName);
        localStorage.setItem(TOKEN_KEY, auth.access_token);
        view = "lobby";
        notice = null;
        serviceReachability = "checking";
        show();
        void refreshServiceStatus();
      } catch (error) {
        setNotice("error", describeAuthError(error, authMode));
      } finally {
        setBusy(false);
        show();
      }
    });
    document.getElementById("logout")?.addEventListener("click", () => {
      stopRoomPing();
      localStorage.removeItem(USER_KEY);
      localStorage.removeItem(TOKEN_KEY);
      currentRoom = null;
      view = "auth";
      show();
    });
    document.getElementById("create-room")?.addEventListener("click", async () => {
      setBusy(true);
      show();
      try {
        const room = await invoke<RoomResponse>("create_room", { settings: settings() });
        await joinRoom(room.room_code, true);
      } catch (error) {
        setNotice("error", `创建房间失败：${describeError(error)}`);
        setBusy(false);
        show();
      }
    });
    document.getElementById("join-room")?.addEventListener("click", () => {
      void joinRoom(inputValue("room-code"), false);
    });
    document.querySelectorAll<HTMLElement>("[data-room-code]").forEach((element) => {
      element.addEventListener("click", () => {
        void joinRoom(element.dataset.roomCode ?? "", false);
      });
    });
    document.getElementById("back-lobby")?.addEventListener("click", () => {
      stopRoomPing();
      view = "lobby";
      currentRoom = null;
      roomPing = null;
      notice = null;
      show();
    });
    document.getElementById("leave-room")?.addEventListener("click", () => {
      stopRoomPing();
      view = "lobby";
      currentRoom = null;
      roomPing = null;
      notice = null;
      show();
    });
    document.getElementById("copy-room")?.addEventListener("click", async () => {
      if (!currentRoom) return;
      try {
        await navigator.clipboard.writeText(currentRoom.code);
        setNotice("success", "房间码已复制。");
      } catch {
        setNotice("error", "复制失败，请手动记下房间码。");
      }
      show();
    });
  };

  show();
  if (view === "lobby") void refreshServiceStatus();
}

export function mountBridgeApp(options: BridgeUiOptions): void {
  render(options);
}
