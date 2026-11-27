// Shows what the bridge reports. Everything else happens in the Rust part of the app.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const DISCORD_STATUS = {
  disconnected: "Discord가 실행 중이 아니거나 연결할 수 없습니다.",
  connecting: "연결하는 중",
  authorizing: "Discord에서 권한 요청을 승인해 주세요.",
  unauthorized: "권한이 없거나 로그인하는 데 실패하였습니다.",
  connected: "연결됨",
};

const element = (id) => document.getElementById(id);

function describeChannel({ discord, channel, members }) {
  if (channel) return `${channel.name || "이름 없는 채널"} · ${members.length}명`;
  return discord === "connected" ? "참여 중인 채널 없음" : "-";
}

function show(status) {
  element("discord").textContent = DISCORD_STATUS[status.discord] ?? status.discord;
  element("channel").textContent = describeChannel(status);
  element("clients").textContent = `${status.clients}개`;

  // The pages find the bridge on one fixed port, so nothing works while another program has it.
  element("error").hidden = !status.serverError;
  element("error").textContent = status.serverError
    ? `오버레이가 연결할 포트를 열 수 없습니다. 다른 프로그램이 사용 중인지 확인해 주세요. (${status.serverError})`
    : "";

  element("authorize").hidden = status.discord !== "unauthorized";
  element("forget").hidden = status.discord !== "connected";
}

element("authorize").addEventListener("click", () => invoke("request_authorization"));
element("forget").addEventListener("click", () => invoke("forget_authorization"));

// Listen first, so that no change falls between asking for the status and following it.
listen("status", (event) => show(event.payload)).then(() => invoke("get_status").then(show));
