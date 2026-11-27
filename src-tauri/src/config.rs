//! What the bridge is built to talk to. Environment variables named `COLLABOT_BRIDGE_*` replace
//! parts of it, which is how a development build points at another site or at a stand-in for
//! Discord.

use std::path::PathBuf;

/// The port the pages find the bridge on, on this computer only. It is fixed because a web page
/// has no way to ask where the bridge listens. The pages name the same port in `bridge.ts`.
pub const SERVER_PORT: u16 = 46463;

/// The sites whose pages may connect, as the `Origin` their browser sends. A browser sends it
/// with every WebSocket connection and a page cannot change it, so this keeps every other website
/// from reading who is in the voice channel.
const ALLOWED_ORIGINS: &[&str] = &["https://overlay.colla.bot", "http://localhost:3000"];

// The Discord application the bridge signs in as: its client ID, and the service that exchanges its
// codes for tokens. Discord only gives the `rpc` scope to applications it has approved.
const DISCORD_CLIENT_ID: &str = env!("COLLABOT_BRIDGE_DISCORD_CLIENT_ID");
const DISCORD_TOKEN_URL: &str = env!("COLLABOT_BRIDGE_DISCORD_TOKEN_URL");

/// What the bridge asks the user to allow: `rpc` to learn who is in the voice channel and who
/// speaks, `identify` to tell which of them is the user.
pub const DISCORD_SCOPES: &[&str] = &["identify", "rpc"];

fn variable(name: &str) -> Option<String> {
    std::env::var(format!("COLLABOT_BRIDGE_{name}"))
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|item| item.trim().trim_end_matches('/').to_owned())
        .filter(|item| !item.is_empty())
        .collect()
}

/// The origins of the pages that may connect. `COLLABOT_BRIDGE_ORIGINS` adds to them, as a list
/// separated by commas: a site of your own, or a development server on another port.
pub fn allowed_origins() -> Vec<String> {
    let mut origins: Vec<String> = ALLOWED_ORIGINS
        .iter()
        .map(|&origin| origin.to_owned())
        .collect();
    origins.extend(variable("ORIGINS").iter().flat_map(|value| list(value)));
    origins
}

/// How to reach Discord and as which application.
pub struct DiscordApp {
    pub client_id: String,
    /// Takes `{"code": …}` by POST and answers with `{"access_token": …}`.
    pub token_url: String,
    /// The pipes to look for Discord's client at, in this order.
    pub ipc_paths: Vec<String>,
}

/// The application the bridge was built with and the pipes of this system, or what
/// `COLLABOT_BRIDGE_DISCORD_CLIENT_ID`, `…_DISCORD_TOKEN_URL` and `…_DISCORD_IPC_PATHS` name in
/// their place when it runs.
pub fn discord_app() -> DiscordApp {
    DiscordApp {
        client_id: variable("DISCORD_CLIENT_ID").unwrap_or_else(|| DISCORD_CLIENT_ID.to_owned()),
        token_url: variable("DISCORD_TOKEN_URL").unwrap_or_else(|| DISCORD_TOKEN_URL.to_owned()),
        ipc_paths: variable("DISCORD_IPC_PATHS")
            .map_or_else(crate::ipc::default_paths, |value| list(&value)),
    }
}

/// Where to keep the token instead of the application's own folder, from
/// `COLLABOT_BRIDGE_DATA_DIR`. Tests use it to leave the real token alone.
pub fn data_dir() -> Option<PathBuf> {
    variable("DATA_DIR").map(PathBuf::from)
}
