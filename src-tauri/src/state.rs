//! What the bridge knows, shared between the task that follows Discord, the server that tells the
//! pages, and the window.

use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{mpsc, watch};

/// The version of the messages the pages get. A page shows nothing for a version it does not know.
const PROTOCOL_VERSION: u32 = 1;

/// How far the bridge has got with Discord.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DiscordStatus {
    /// Discord's client is not running, or the bridge cannot connect to it: it did not answer in
    /// time, or it turned the bridge away before greeting it.
    Disconnected,
    /// The client answered and is being signed in to.
    Connecting,
    /// Discord is showing the user its consent screen, going by how long it has left a request for
    /// permission unanswered.
    Authorizing,
    /// No permission, or signing in failed: the user declined or took the permission back, the
    /// token service gave no token, or Discord refused the token. The bridge waits for the user to
    /// ask again.
    Unauthorized,
    /// Signed in: the voice channel and its members are known from here on.
    Connected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub id: String,
    /// The name the voice channel's server shows for the member.
    pub name: String,
    pub global_name: Option<String>,
    pub username: String,
    pub bot: bool,
    pub speaking: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Channel {
    pub id: String,
    pub name: String,
}

/// Everything the pages are told, sent whole whenever any of it changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub v: u32,
    pub discord: DiscordStatus,
    /// The user's own Discord ID, which is also among the members while in a voice channel.
    pub self_id: Option<String>,
    pub channel: Option<Channel>,
    pub members: Vec<Member>,
}

impl Snapshot {
    fn new() -> Self {
        Self {
            v: PROTOCOL_VERSION,
            discord: DiscordStatus::Disconnected,
            self_id: None,
            channel: None,
            members: Vec::new(),
        }
    }

    /// Forgets the voice channel, as when it was left or the connection to Discord ended.
    pub fn leave_channel(&mut self) {
        self.channel = None;
        self.members.clear();
    }
}

/// What only the window shows: how the server for the pages is doing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Local {
    /// How many pages are connected.
    pub clients: usize,
    /// Why the pages cannot connect at all, when they cannot.
    pub server_error: Option<String>,
}

/// What the task that follows Discord is asked to do.
#[derive(Debug)]
pub enum Command {
    /// Try again now instead of waiting: the user pressed connect on the dashboard.
    Wake,
    /// Show Discord's consent screen again after it was declined.
    Authorize,
    /// Drop the token and start over, unauthorized until the user asks for permission again.
    Forget,
}

pub struct Shared {
    snapshot: watch::Sender<Snapshot>,
    local: watch::Sender<Local>,
    commands: mpsc::UnboundedSender<Command>,
}

impl Shared {
    /// Returns the shared state and the receiving end of its commands, for the Discord task.
    pub fn new() -> (Arc<Self>, mpsc::UnboundedReceiver<Command>) {
        let (commands, receiver) = mpsc::unbounded_channel();
        let shared = Self {
            snapshot: watch::Sender::new(Snapshot::new()),
            local: watch::Sender::new(Local::default()),
            commands,
        };
        (Arc::new(shared), receiver)
    }

    pub fn snapshot(&self) -> watch::Receiver<Snapshot> {
        self.snapshot.subscribe()
    }

    pub fn local(&self) -> watch::Receiver<Local> {
        self.local.subscribe()
    }

    /// Changes the snapshot. Those following it only hear of it when something really changed.
    pub fn update(&self, change: impl FnOnce(&mut Snapshot)) {
        self.snapshot.send_if_modified(|snapshot| {
            let before = snapshot.clone();
            change(snapshot);
            *snapshot != before
        });
    }

    pub fn update_local(&self, change: impl FnOnce(&mut Local)) {
        self.local.send_if_modified(|local| {
            let before = local.clone();
            change(local);
            *local != before
        });
    }

    pub fn send(&self, command: Command) {
        // Fails only once the Discord task is gone, which is when the bridge is closing.
        let _ = self.commands.send(command);
    }
}
