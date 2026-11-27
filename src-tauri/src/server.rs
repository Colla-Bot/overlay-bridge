//! Tells the overlay and the dashboard what the bridge knows, over WebSocket connections they
//! open to this computer.

use std::{sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::{
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_tungstenite::{
    accept_hdr_async_with_config,
    tungstenite::{
        handshake::server::{ErrorResponse, Request, Response},
        http::StatusCode,
        protocol::WebSocketConfig,
        Message,
    },
};

use crate::{
    config::SERVER_PORT,
    state::{Command, Shared},
};

/// More pages than a streaming program ever has open at once.
const MAX_CLIENTS: usize = 16;
/// A connection that does not finish its handshake in this time is dropped.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// The pages only send short requests.
const MAX_MESSAGE_SIZE: usize = 1024;

/// What a page may ask for.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ClientRequest {
    /// Have Discord ask the user for permission again.
    Authorize,
}

/// Counts a page as connected for as long as this lives.
struct Client<'a>(&'a Shared);

impl<'a> Client<'a> {
    fn new(shared: &'a Shared) -> Self {
        shared.update_local(|local| local.clients += 1);
        Self(shared)
    }
}

impl Drop for Client<'_> {
    fn drop(&mut self) {
        self.0.update_local(|local| local.clients -= 1);
    }
}

async fn serve(stream: TcpStream, shared: Arc<Shared>, origins: Arc<Vec<String>>) {
    // Any website can try to connect to a port of this computer, and what the bridge tells is not
    // for all of them. The browser names the site in `Origin`, which a page cannot forge.
    // (The library sets the type of the refusal, large as it is.)
    #[allow(clippy::result_large_err)]
    let check_origin = |request: &Request, response: Response| {
        let origin = request
            .headers()
            .get("origin")
            .and_then(|value| value.to_str().ok());
        if origin.is_some_and(|origin| origins.iter().any(|allowed| allowed == origin)) {
            Ok(response)
        } else {
            let mut refusal = ErrorResponse::new(None);
            *refusal.status_mut() = StatusCode::FORBIDDEN;
            Err(refusal)
        }
    };
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_SIZE))
        .max_frame_size(Some(MAX_MESSAGE_SIZE));
    let handshake = accept_hdr_async_with_config(stream, check_origin, Some(config));
    let Ok(Ok(mut socket)) = timeout(HANDSHAKE_TIMEOUT, handshake).await else {
        return;
    };
    if shared.local().borrow().clients >= MAX_CLIENTS {
        return;
    }
    let _client = Client::new(&shared);

    // The snapshot as it is now, and again whenever it changes. A page that falls behind skips
    // to the latest one.
    let mut snapshots = shared.snapshot();
    loop {
        let Ok(text) = serde_json::to_string(&*snapshots.borrow_and_update()) else {
            return;
        };
        if socket.send(Message::text(text)).await.is_err() {
            return;
        }
        loop {
            tokio::select! {
                changed = snapshots.changed() => match changed {
                    Ok(()) => break,
                    Err(_) => return,
                },
                message = socket.next() => match message {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(ClientRequest::Authorize) = serde_json::from_str(&text) {
                            shared.send(Command::Authorize);
                        }
                    }
                    // Pings are answered by the library, when the next message is sent or read.
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    // Closed, broken, or sending what no page of ours sends.
                    _ => return,
                },
            }
        }
    }
}

/// Listens on this computer only, for as long as the bridge runs.
pub async fn run(shared: Arc<Shared>, origins: Vec<String>) {
    let listener = match TcpListener::bind(("127.0.0.1", SERVER_PORT)).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("Could not listen on port {SERVER_PORT}: {error}");
            shared.update_local(|local| local.server_error = Some(error.to_string()));
            return;
        }
    };
    let origins = Arc::new(origins);
    loop {
        if let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve(stream, shared.clone(), origins.clone()));
        }
    }
}
