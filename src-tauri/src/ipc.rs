//! The pipe Discord's client serves its RPC on: a named pipe on Windows, a Unix socket elsewhere.
//!
//! What goes over it is the same JSON as over Discord's WebSocket, in frames: an opcode and the
//! length of what follows, four bytes each and little-endian, then that many bytes of JSON.

use std::io;

use tokio_util::{
    bytes::{Buf, BufMut, BytesMut},
    codec::{Decoder, Encoder, Framed},
};

/// Opens a connection: `{"v": 1, "client_id": …}`. Discord answers it with its greeting.
pub const HANDSHAKE: u32 = 0;
/// A command, an answer or an event.
pub const FRAME: u32 = 1;
/// The other side is ending the connection, and says why: `{"code": …, "message": …}`.
pub const CLOSE: u32 = 2;
pub const PING: u32 = 3;
pub const PONG: u32 = 4;

/// Discord sets no limit to what it sends, and what it says of a channel names every member, at
/// some hundreds of bytes each: a Stage channel with thousands listening takes mebibytes. A frame
/// longer than this means the two sides have lost step.
const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;
const HEADER_SIZE: usize = 8;

/// The places a client of Discord may serve its pipe at: the first of ten that is free.
const PLACES: std::ops::Range<u8> = 0..10;

#[cfg(windows)]
type Stream = tokio::net::windows::named_pipe::NamedPipeClient;
#[cfg(unix)]
type Stream = tokio::net::UnixStream;

/// A connection that gives and takes whole frames.
pub type Pipe = Framed<Stream, Codec>;

pub struct Frame {
    pub opcode: u32,
    pub body: String,
}

pub struct Codec;

impl Decoder for Codec {
    type Item = Frame;
    type Error = io::Error;

    fn decode(&mut self, buffer: &mut BytesMut) -> io::Result<Option<Frame>> {
        let Some(header) = buffer.first_chunk::<HEADER_SIZE>() else {
            return Ok(None);
        };
        let opcode = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        let length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        if length > MAX_FRAME_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("a frame of {length} bytes"),
            ));
        }
        if buffer.len() < HEADER_SIZE + length {
            buffer.reserve(HEADER_SIZE + length - buffer.len());
            return Ok(None);
        }
        buffer.advance(HEADER_SIZE);
        let body = String::from_utf8_lossy(&buffer.split_to(length)).into_owned();
        Ok(Some(Frame { opcode, body }))
    }
}

impl Encoder<Frame> for Codec {
    type Error = io::Error;

    // The whole frame goes into the buffer at once, and so to the pipe in one piece.
    fn encode(&mut self, frame: Frame, buffer: &mut BytesMut) -> io::Result<()> {
        let length = u32::try_from(frame.body.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a frame too long to send"))?;
        buffer.reserve(HEADER_SIZE + frame.body.len());
        buffer.put_u32_le(frame.opcode);
        buffer.put_u32_le(length);
        buffer.put_slice(frame.body.as_bytes());
        Ok(())
    }
}

/// Opens the pipe at `path`, if a program serves one there.
// Opening a named pipe takes no waiting, unlike connecting to a socket.
#[cfg_attr(windows, allow(clippy::unused_async))]
pub async fn open(path: &str) -> io::Result<Pipe> {
    #[cfg(windows)]
    let stream = tokio::net::windows::named_pipe::ClientOptions::new().open(path)?;
    #[cfg(unix)]
    let stream = tokio::net::UnixStream::connect(path).await?;
    Ok(Framed::new(stream, Codec))
}

/// Where to look for Discord's pipe on this system, in the order to try.
#[cfg(windows)]
pub fn default_paths() -> Vec<String> {
    PLACES
        .map(|place| format!(r"\\?\pipe\discord-ipc-{place}"))
        .collect()
}

/// Where to look for Discord's pipe on this system, in the order to try.
#[cfg(unix)]
pub fn default_paths() -> Vec<String> {
    // Discord serves it in the first of these folders that its environment names, which need not
    // be this program's environment, and in `/tmp` if it names none.
    let mut folders: Vec<String> = ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|folder| folder.trim_end_matches('/').to_owned())
        .filter(|folder| !folder.is_empty())
        .collect();
    folders.push("/tmp".to_owned());
    folders.dedup();
    // A client packaged as a Flatpak or a Snap has its folder inside that one.
    const PACKAGES: [&str; 3] = ["", "app/com.discordapp.Discord/", "snap.discord/"];
    folders
        .iter()
        .flat_map(|folder| PACKAGES.map(|package| format!("{folder}/{package}")))
        .flat_map(|folder| PLACES.map(move |place| format!("{folder}discord-ipc-{place}")))
        .collect()
}
