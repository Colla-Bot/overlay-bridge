//! Follows Discord's client through its local RPC server, over the pipe the client serves it on:
//! which voice channel the user is in, who else is there, and who is speaking.
//!
//! A Rust port of Overlayed's RPC manager, `apps/desktop/src/rpc/manager.ts`
//! (<https://github.com/overlayeddev/overlayed>): the commands and their order, and how it
//! reconnects, keeps the voice channel in sync and asks for permission again, are Overlayed's.
//! What has changed since, such as reaching Discord over its pipe rather than its WebSocket and
//! looking in more than one place, is the bridge's own.

use std::{
    collections::{HashSet, VecDeque},
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{
    sync::mpsc::UnboundedReceiver,
    time::{sleep_until, Instant},
};

use crate::{
    config::{DiscordApp, DISCORD_SCOPES},
    ipc::{self, Frame, Pipe},
    state::{Channel, Command, DiscordStatus, Member, Shared, Snapshot},
    tokens::TokenStore,
};

/// How long to wait before looking for Discord again, by how many attempts failed in a row.
const RECONNECT_DELAYS: [Duration; 4] = [
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(30),
];
/// Discord makes an application that connects too often wait for its greeting. How often that
/// is depends on the application, and anything else on this computer that connects as the same
/// one draws on the same allowance.
const CONNECTIONS_PER_MINUTE: usize = 10;
const MINUTE: Duration = Duration::from_mins(1);
/// How often to ask for the voice channel again, which mends anything an event did not tell and
/// notices a connection that died silently.
const RESYNC_INTERVAL: Duration = Duration::from_secs(20);
const RESYNC_TIMEOUT: Duration = Duration::from_secs(5);
const TOKEN_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a request for permission may go unanswered before the pages are told that Discord is
/// asking the user. Where it needs nobody's say, Discord answers sooner.
const CONSENT_SCREEN_AFTER: Duration = Duration::from_secs(2);
/// How long Discord may take to greet or to take a token: longer than its throttle's 30 s wait.
const SIGN_IN_TIMEOUT: Duration = Duration::from_mins(1);

/// The events of a voice channel. Each is subscribed to with the channel's ID.
const CHANNEL_EVENTS: [&str; 5] = [
    "SPEAKING_START",
    "SPEAKING_STOP",
    "VOICE_STATE_CREATE",
    "VOICE_STATE_DELETE",
    "VOICE_STATE_UPDATE",
];

/// The errors with which Discord refuses a token that asking for a new one mends: an unknown
/// application, token or user, and anything OAuth2 has to say.
const REAUTHORIZE_ERRORS: [i64; 4] = [4007, 4009, 4010, 5000];
/// The code Discord closes a connection with when the user takes the application's permission
/// back in Discord's settings.
const TOKEN_REVOKED: i64 = 4003;
/// The user in the greeting of arRPC, a stand-in for Discord's RPC that other clients bring. It
/// takes a pipe as Discord does and greets, but answers nothing about signing in or voice.
const STAND_IN_USER: &str = "1045800378228281345";

/// Why a session ended.
enum End {
    /// The connection is gone, or no longer to be trusted.
    Lost(String),
    /// The user had the token dropped.
    Forgotten,
    /// The bridge is closing.
    Closing,
}

/// A request for the voice channel that has not been answered yet.
struct Resync {
    nonce: String,
    deadline: Instant,
    /// Whether this is the second request in a row, the first having gone unanswered.
    repeated: bool,
}

/// One connection to Discord, from its greeting to its end.
struct Session<'a> {
    socket: Pipe,
    shared: &'a Shared,
    app: &'a DiscordApp,
    tokens: &'a TokenStore,
    http: &'a reqwest::Client,
    /// Whether Discord may show its consent screen without the user having asked for it.
    may_prompt: &'a mut bool,
    nonces: u64,
    /// Whether Discord has greeted the connection.
    greeted: bool,
    authenticated: bool,
    /// Whether permission was already asked for again because Discord refused the saved token.
    reauthorized: bool,
    /// The voice channel whose events are subscribed to.
    channel_id: Option<String>,
    resync: Option<Resync>,
    next_resync: Instant,
    /// When a request for permission that is still unanswered will have waited long enough to
    /// tell that Discord is asking the user.
    consent_due: Option<Instant>,
    sign_in_by: Instant,
}

impl Session<'_> {
    async fn run(&mut self, commands: &mut UnboundedReceiver<Command>) -> End {
        loop {
            let deadline = self
                .resync
                .as_ref()
                .map_or(self.next_resync, |resync| resync.deadline);
            let consent_due = self.consent_due;
            let signing_in = !self.authenticated && self.status() == DiscordStatus::Connecting;
            let step = tokio::select! {
                frame = self.socket.next() => match frame {
                    Some(Ok(Frame { opcode: ipc::FRAME, body })) => self.handle(&body).await,
                    Some(Ok(Frame { opcode: ipc::PING, body })) => {
                        self.write(ipc::PONG, body).await
                    }
                    Some(Ok(Frame { opcode: ipc::CLOSE, body })) => Err(self.closed(&body)),
                    Some(Ok(_)) => Ok(()),
                    Some(Err(error)) => Err(End::Lost(error.to_string())),
                    None => Err(End::Lost("the connection ended".to_owned())),
                },
                command = commands.recv() => match command {
                    Some(command) => self.obey(command).await,
                    None => Err(End::Closing),
                },
                () = sleep_until(deadline), if self.authenticated => self.keep_in_sync().await,
                () = sleep_until(consent_due.unwrap_or(deadline)), if consent_due.is_some() => {
                    // Discord would have answered by itself by now, so its consent screen is up.
                    self.consent_due = None;
                    self.set_status(DiscordStatus::Authorizing);
                    Ok(())
                }
                () = sleep_until(self.sign_in_by), if signing_in => {
                    Err(End::Lost("Discord took too long to sign the bridge in".to_owned()))
                }
            };
            if let Err(end) = step {
                return end;
            }
        }
    }

    fn status(&self) -> DiscordStatus {
        self.shared.snapshot().borrow().discord
    }

    fn set_status(&self, status: DiscordStatus) {
        self.shared.update(|snapshot| snapshot.discord = status);
    }

    async fn write(&mut self, opcode: u32, body: String) -> Result<(), End> {
        self.socket
            .send(Frame { opcode, body })
            .await
            .map_err(|error| End::Lost(error.to_string()))
    }

    /// Takes Discord's word for why it ends the connection: `{"code": …, "message": …}`.
    fn closed(&mut self, body: &str) -> End {
        let reason: Value = serde_json::from_str(body).unwrap_or_default();
        if reason["code"].as_i64() == Some(TOKEN_REVOKED) {
            // The user said no in Discord itself. Asking again is theirs to do.
            self.tokens.clear();
            *self.may_prompt = false;
            return End::Forgotten;
        }
        End::Lost(format!("Discord closed the connection: {reason}"))
    }

    /// Sends a command and returns the nonce Discord will answer it with.
    async fn send(
        &mut self,
        cmd: &str,
        args: Option<Value>,
        evt: Option<&str>,
    ) -> Result<String, End> {
        self.nonces += 1;
        let nonce = format!("collabot-{}", self.nonces);
        let mut payload = json!({ "cmd": cmd, "nonce": nonce });
        if let Some(args) = args {
            payload["args"] = args;
        }
        if let Some(evt) = evt {
            payload["evt"] = json!(evt);
        }
        self.write(ipc::FRAME, payload.to_string()).await?;
        Ok(nonce)
    }

    /// Asks Discord for permission to look at the user's voice channel.
    ///
    /// The request says `prompt: none`. Discord then answers by itself for a user who agreed
    /// before and has not taken that back, which is how a token that ran out is replaced
    /// unnoticed, and shows its consent screen to anyone else. Without it, Discord shows the
    /// screen every time, also to those who agreed before.
    async fn authorize(&mut self) -> Result<(), End> {
        let args = json!({
            "client_id": self.app.client_id,
            "scopes": DISCORD_SCOPES,
            "prompt": "none",
        });
        self.send("AUTHORIZE", Some(args), None).await?;
        // Which of the two it is, nothing tells beforehand, and Discord answering by itself takes
        // a moment too. So the pages go on saying that the bridge is connecting, and are told to
        // approve the request only once Discord has let it wait.
        self.set_status(DiscordStatus::Connecting);
        self.consent_due = Some(Instant::now() + CONSENT_SCREEN_AFTER);
        self.sign_in_by = Instant::now() + SIGN_IN_TIMEOUT;
        Ok(())
    }

    async fn authenticate(&mut self, access_token: &str) -> Result<(), End> {
        self.sign_in_by = Instant::now() + SIGN_IN_TIMEOUT;
        self.send(
            "AUTHENTICATE",
            Some(json!({ "access_token": access_token })),
            None,
        )
        .await?;
        Ok(())
    }

    /// Leaves the user unauthorized until they ask again: asking by itself, over and over, would
    /// keep putting a consent screen in front of someone who just closed one.
    fn give_up_authorizing(&mut self) {
        *self.may_prompt = false;
        self.set_status(DiscordStatus::Unauthorized);
    }

    /// Asks which voice channel the user is in. The answer comes as a message of its own.
    async fn request_channel(&mut self) -> Result<String, End> {
        self.send("GET_SELECTED_VOICE_CHANNEL", None, None).await
    }

    async fn channel_events(&mut self, cmd: &str, channel_id: &str) -> Result<(), End> {
        for evt in CHANNEL_EVENTS {
            self.send(cmd, Some(json!({ "channel_id": channel_id })), Some(evt))
                .await?;
        }
        Ok(())
    }

    /// Stops listening to the voice channel and forgets who was in it.
    async fn leave_channel(&mut self) -> Result<(), End> {
        if let Some(channel_id) = self.channel_id.take() {
            self.channel_events("UNSUBSCRIBE", &channel_id).await?;
        }
        self.shared.update(Snapshot::leave_channel);
        Ok(())
    }

    async fn obey(&mut self, command: Command) -> Result<(), End> {
        match command {
            Command::Forget => {
                self.tokens.clear();
                *self.may_prompt = false;
                // Discord goes on answering this connection whatever happens to the token.
                Err(End::Forgotten)
            }
            Command::Authorize | Command::Wake => {
                *self.may_prompt = true;
                if self.status() == DiscordStatus::Unauthorized {
                    self.authorize().await?;
                }
                Ok(())
            }
        }
    }

    /// Asks for the voice channel again when that is due, and gives the connection up when the
    /// last two requests went unanswered.
    async fn keep_in_sync(&mut self) -> Result<(), End> {
        let repeated = match self.resync.take() {
            Some(Resync { repeated: true, .. }) => {
                return Err(End::Lost("Discord stopped answering".to_owned()));
            }
            // Once more right away, so that a dead connection does not show stale members for
            // another whole interval.
            Some(_) => true,
            None => false,
        };
        let nonce = self.request_channel().await?;
        let deadline = Instant::now() + RESYNC_TIMEOUT;
        self.resync = Some(Resync {
            nonce,
            deadline,
            repeated,
        });
        Ok(())
    }

    async fn handle(&mut self, text: &str) -> Result<(), End> {
        let Ok(payload) = serde_json::from_str::<Value>(text) else {
            return Ok(());
        };
        let cmd = payload["cmd"].as_str().unwrap_or_default();
        let evt = payload["evt"].as_str().unwrap_or_default();
        let failed = evt == "ERROR";
        let data = &payload["data"];

        match (cmd, evt) {
            // Discord's greeting. A saved token signs in with no question asked.
            (_, "READY") => {
                // Over the pipe the greeting names the user. A program that names none, or the
                // made-up one, is not a Discord that tells of voice channels.
                let user = data["user"]["id"].as_str().unwrap_or("0");
                if user == "0" || user == STAND_IN_USER {
                    return Err(End::Lost("what greeted was not Discord".to_owned()));
                }
                self.greeted = true;
                match self.tokens.load() {
                    Some(token) => self.authenticate(&token).await?,
                    None if *self.may_prompt => self.authorize().await?,
                    None => self.set_status(DiscordStatus::Unauthorized),
                }
            }

            // Discord's answer, its own or the user's on the consent screen: a code to exchange
            // for a token, or none.
            ("AUTHORIZE", _) => {
                self.consent_due = None;
                match data["code"].as_str().filter(|_| !failed) {
                    Some(code) => {
                        // Nothing is left for the user to approve while the code is exchanged.
                        self.set_status(DiscordStatus::Connecting);
                        match self.exchange(code).await {
                            Ok(token) => {
                                self.tokens.save(&token);
                                self.authenticate(&token).await?;
                            }
                            Err(error) => {
                                eprintln!("Could not get a token for Discord: {error}");
                                self.give_up_authorizing();
                            }
                        }
                    }
                    None => {
                        // Discord's reason, which is the user's no more often than not.
                        eprintln!("Discord gave no permission: {}", data["message"]);
                        self.give_up_authorizing();
                    }
                }
            }

            ("AUTHENTICATE", _) if failed => {
                let code = data["code"].as_i64().unwrap_or_default();
                // Discord's message quotes the token, so only the code is logged.
                eprintln!("Discord refused the token with error {code}");
                if REAUTHORIZE_ERRORS.contains(&code) {
                    self.tokens.clear();
                    if *self.may_prompt && !self.reauthorized {
                        self.reauthorized = true;
                        // The user agreed when this token was made, so Discord gives a new one
                        // without asking them, unless they have taken that back since.
                        return self.authorize().await;
                    }
                }
                self.give_up_authorizing();
            }
            ("AUTHENTICATE", _) => {
                self.authenticated = true;
                self.next_resync = Instant::now() + RESYNC_INTERVAL;
                let self_id = data["user"]["id"].as_str().map(str::to_owned);
                self.shared.update(|snapshot| {
                    snapshot.discord = DiscordStatus::Connected;
                    snapshot.self_id = self_id;
                });
                // Tells when the user joins, changes or leaves a voice channel.
                self.send("SUBSCRIBE", None, Some("VOICE_CHANNEL_SELECT"))
                    .await?;
                self.request_channel().await?;
            }

            // The channel the event names is asked about, and the answer taken for where the user
            // is.
            (_, "VOICE_CHANNEL_SELECT") => match data["channel_id"].as_str() {
                Some(channel_id) => {
                    let args = json!({ "channel_id": channel_id });
                    self.send("GET_CHANNEL", Some(args), None).await?;
                }
                None => self.leave_channel().await?,
            },
            ("GET_CHANNEL", _) if failed => {
                self.request_channel().await?;
            }
            ("GET_CHANNEL", _) => self.reconcile(data).await?,

            ("GET_SELECTED_VOICE_CHANNEL", _) => {
                let nonce = payload["nonce"].as_str();
                if self
                    .resync
                    .as_ref()
                    .is_some_and(|resync| nonce == Some(resync.nonce.as_str()))
                {
                    self.resync = None;
                    self.next_resync = Instant::now() + RESYNC_INTERVAL;
                    if failed {
                        return Err(End::Lost(
                            "Discord could not tell the voice channel".to_owned(),
                        ));
                    }
                }
                if !failed {
                    self.reconcile(data).await?;
                }
            }

            (_, "VOICE_STATE_CREATE" | "VOICE_STATE_UPDATE") => {
                if let Some(member) = parse_member(data) {
                    self.shared.update(|snapshot| {
                        match snapshot
                            .members
                            .iter_mut()
                            .find(|known| known.id == member.id)
                        {
                            Some(known) => {
                                *known = Member {
                                    speaking: known.speaking,
                                    ..member
                                }
                            }
                            None => snapshot.members.push(member),
                        }
                    });
                }
            }
            (_, "VOICE_STATE_DELETE") => {
                let Some(id) = data["user"]["id"].as_str() else {
                    return Ok(());
                };
                if self.shared.snapshot().borrow().self_id.as_deref() == Some(id) {
                    // The user left, or was moved by somebody else, of which Discord tells
                    // nothing more. So ask where they are now.
                    self.leave_channel().await?;
                    self.request_channel().await?;
                } else {
                    self.shared
                        .update(|snapshot| snapshot.members.retain(|member| member.id != id));
                }
            }

            (_, "SPEAKING_START" | "SPEAKING_STOP") => {
                let speaking = evt == "SPEAKING_START";
                if let Some(id) = data["user_id"].as_str() {
                    self.shared.update(|snapshot| {
                        if let Some(member) =
                            snapshot.members.iter_mut().find(|member| member.id == id)
                        {
                            member.speaking = speaking;
                        }
                    });
                }
            }

            _ if failed => eprintln!("Discord answered {cmd} with an error: {}", data["message"]),
            _ => {}
        }
        Ok(())
    }

    /// Takes Discord's word for which voice channel the user is in and who is there.
    async fn reconcile(&mut self, data: &Value) -> Result<(), End> {
        let Some(id) = data["id"].as_str() else {
            return self.leave_channel().await;
        };
        if self.channel_id.as_deref() != Some(id) {
            if let Some(previous) = self.channel_id.take() {
                self.channel_events("UNSUBSCRIBE", &previous).await?;
            }
            self.channel_events("SUBSCRIBE", id).await?;
            self.channel_id = Some(id.to_owned());
        }

        let channel = Channel {
            id: id.to_owned(),
            name: data["name"].as_str().unwrap_or_default().to_owned(),
        };
        let members: Vec<Member> = data["voice_states"]
            .as_array()
            .map(|states| states.iter().filter_map(parse_member).collect())
            .unwrap_or_default();
        self.shared.update(|snapshot| {
            // The answer does not say who is speaking: only events do. In the same channel those
            // who were speaking still are, and a new channel starts silent.
            let same_channel = snapshot
                .channel
                .as_ref()
                .is_some_and(|known| known.id == channel.id);
            let speaking: HashSet<String> = snapshot
                .members
                .iter()
                .filter(|member| same_channel && member.speaking)
                .map(|member| member.id.clone())
                .collect();
            snapshot.channel = Some(channel);
            snapshot.members = members
                .into_iter()
                .map(|member| Member {
                    speaking: speaking.contains(&member.id),
                    ..member
                })
                .collect();
        });
        Ok(())
    }

    /// Exchanges the code Discord gave for an access token. That takes the application's secret,
    /// which is why a server does it.
    async fn exchange(&self, code: &str) -> Result<String, String> {
        // A service may answer a code it will not take with an empty object, not with an error.
        #[derive(Deserialize)]
        struct Token {
            #[serde(default)]
            access_token: String,
        }

        let response = self
            .http
            .post(&self.app.token_url)
            .json(&json!({ "code": code }))
            .send()
            .await
            .map_err(|error| error.to_string())?;
        if !response.status().is_success() {
            return Err(format!("the token service answered {}", response.status()));
        }
        let token: Token = response.json().await.map_err(|error| error.to_string())?;
        let access_token = token.access_token.trim();
        if access_token.is_empty() {
            return Err("the token service answered without a token".to_owned());
        }
        Ok(access_token.to_owned())
    }
}

/// Reads a member from what Discord calls a voice state.
fn parse_member(state: &Value) -> Option<Member> {
    let text = |value: &Value| {
        value
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let user = &state["user"];
    let id = text(&user["id"])?;
    let username = text(&user["username"]).unwrap_or_default();
    let global_name = text(&user["global_name"]);
    Some(Member {
        id,
        // The nickname in the channel's server, where the user has one.
        name: text(&state["nick"])
            .or_else(|| global_name.clone())
            .unwrap_or_else(|| username.clone()),
        global_name,
        username,
        bot: user["bot"].as_bool().unwrap_or(false),
        speaking: false,
    })
}

/// Looks for Discord's pipe among the places from the one at `from` on, opens the first that is
/// served and says as which application. Returns which place that was.
async fn connect(app: &DiscordApp, from: usize) -> Option<(usize, Pipe)> {
    let hello = json!({ "v": 1, "client_id": app.client_id }).to_string();
    for (index, path) in app.ipc_paths.iter().enumerate().skip(from) {
        let Ok(mut socket) = ipc::open(path).await else {
            continue;
        };
        let handshake = Frame {
            opcode: ipc::HANDSHAKE,
            body: hello.clone(),
        };
        if socket.send(handshake).await.is_ok() {
            return Some((index, socket));
        }
    }
    None
}

/// Keeps a connection to Discord for as long as the bridge runs, and the shared state up to date
/// with what it says.
pub async fn run(
    shared: std::sync::Arc<Shared>,
    app: DiscordApp,
    tokens: TokenStore,
    mut commands: UnboundedReceiver<Command>,
) {
    let http = reqwest::Client::builder()
        .timeout(TOKEN_TIMEOUT)
        .build()
        .unwrap_or_default();
    let mut may_prompt = true;
    let mut failures = 0;
    // When Discord accepted the latest connections.
    let mut accepted: VecDeque<Instant> = VecDeque::new();
    // Which place to start looking at: the first, unless what answered there was not Discord.
    let mut from = 0;

    loop {
        // What the pages are told while the bridge waits to look for Discord again.
        let mut status = DiscordStatus::Disconnected;
        if let Some((index, socket)) = connect(&app, std::mem::take(&mut from)).await {
            accepted.push_back(Instant::now());
            shared.update(|snapshot| snapshot.discord = DiscordStatus::Connecting);
            let mut session = Session {
                socket,
                shared: &shared,
                app: &app,
                tokens: &tokens,
                http: &http,
                may_prompt: &mut may_prompt,
                nonces: 0,
                greeted: false,
                authenticated: false,
                reauthorized: false,
                channel_id: None,
                resync: None,
                next_resync: Instant::now() + RESYNC_INTERVAL,
                consent_due: None,
                sign_in_by: Instant::now() + SIGN_IN_TIMEOUT,
            };
            let end = session.run(&mut commands).await;
            if session.authenticated {
                failures = 0;
            }
            let greeted = session.greeted;
            match end {
                // Whatever hangs up before greeting, or greets as something else, is not a
                // Discord that talks to the bridge: another program may hold the first pipe, as
                // the one that other clients bring does. Look at the places after it right away,
                // and do not count it.
                End::Lost(_) if !greeted => {
                    accepted.pop_back();
                    from = index + 1;
                    continue;
                }
                End::Lost(reason) => eprintln!("Lost the connection to Discord: {reason}"),
                // Discord is still there, to be signed in to again without the token.
                End::Forgotten => status = DiscordStatus::Connecting,
                End::Closing => return,
            }
        }
        shared.update(|snapshot| {
            snapshot.discord = status;
            snapshot.self_id = None;
            snapshot.leave_channel();
        });

        let now = Instant::now();
        let delay = RECONNECT_DELAYS[failures.min(RECONNECT_DELAYS.len() - 1)];
        failures += 1;
        accepted.retain(|&time| now.duration_since(time) < MINUTE);
        // Attempts that find no Discord cost nothing. Connections Discord accepted are counted.
        let earliest = match accepted.front() {
            Some(&oldest) if accepted.len() >= CONNECTIONS_PER_MINUTE => oldest + MINUTE,
            _ => now,
        };
        let mut retry_at = (now + delay).max(earliest);
        loop {
            tokio::select! {
                () = sleep_until(retry_at) => break,
                command = commands.recv() => match command {
                    Some(Command::Forget) => {
                        tokens.clear();
                        may_prompt = false;
                    }
                    // The user is waiting: try now, or as soon as Discord tolerates.
                    Some(Command::Authorize | Command::Wake) => {
                        may_prompt = true;
                        retry_at = earliest;
                    }
                    None => return,
                },
            }
        }
    }
}
