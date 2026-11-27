# CollaBot Overlay Bridge

A small desktop app that tells the overlay's pages who is speaking in the user's Discord voice
channel. The overlay frames the stream of whoever speaks, and the dashboard shows who is who.

It is a [Tauri](https://tauri.app) app. The part that talks to Discord is a Rust port of the RPC
manager of [Overlayed](https://github.com/overlayeddev/overlayed), which does it in TypeScript.
Like Overlayed it is licensed under the [AGPL 3.0](LICENSE): whoever gets the app from you is
entitled to this source.

## Why a separate app

Discord's desktop client answers questions about the voice channel on a local RPC server. It
serves that on a pipe, which a web page cannot open, and on a WebSocket, which it only lets
connections use whose `Origin` an application has registered with Discord: a web page cannot
choose its `Origin`, and the overlay's site is not registered for anything. So the pages cannot
ask themselves. The bridge asks in their place, over the pipe, and passes on only what they need.

## What it does

- **Starts from a link.** The installer registers the `collabot-overlay://` scheme, and the
  dashboard's connect button opens `collabot-overlay://connect`. Started that way the bridge
  stays in the tray; started by hand it shows a small status window. Closing the window leaves it
  running, and the tray menu quits it. Only one copy runs: a second launch hands its link to the
  first and ends.
- **Follows Discord.** It looks for the client's pipe, `discord-ipc-0` to `discord-ipc-9`, which
  Discord's documentation prefers to the WebSocket, passing over a program that hangs up before
  greeting or greets as something else. It asks the user for permission once (`identify` and
  `rpc`), and from then on knows the voice channel the user is in, its members, and who is
  speaking. Its requests for permission say `prompt: none`, so Discord shows its consent screen
  only to a user who has not agreed before, and a saved token that ran out is replaced
  unnoticed. The pages are told to approve a request only once Discord has left it unanswered for
  two seconds. It asks for the voice channel again every 20 seconds to mend anything an event did
  not tell, and at once when the user is taken out of a channel. It reconnects with growing
  delays and keeps to ten connections a minute.
- **Tells the pages.** It listens on `ws://127.0.0.1:46463`, on this computer only, and sends
  every connected page the whole picture whenever any of it changes:

  ```json
  {
    "v": 1,
    "discord": "connected",
    "selfId": "1000",
    "channel": { "id": "5001", "name": "합방" },
    "members": [
      {
        "id": "2001",
        "name": "앨리스",
        "globalName": "Alice",
        "username": "alice",
        "bot": false,
        "speaking": true
      }
    ]
  }
  ```

  `discord` is `disconnected` (Discord is not running, or the bridge cannot connect to it),
  `connecting`, `authorizing` (the consent screen is open), `unauthorized` (no permission, or
  signing in failed) or `connected`. A page can send one thing, `{"type":"authorize"}`, which has
  the bridge try again at once: look for Discord, or ask Discord for permission again.

Only pages of the sites in `ALLOWED_ORIGINS` ([config.rs](src-tauri/src/config.rs)) may connect:
the browser names the page's site in the `Origin` of every WebSocket handshake, and anything else
is refused with a 403. The token never leaves the bridge.

## The Discord application

| Variable                            | What it is                                    |
| ----------------------------------- | --------------------------------------------- |
| `COLLABOT_BRIDGE_DISCORD_CLIENT_ID` | The application's client ID                   |
| `COLLABOT_BRIDGE_DISCORD_TOKEN_URL` | A service that exchanges its codes for tokens |

Which application it is matters. Discord, according to its
[documentation](https://docs.discord.com/developers/topics/rpc), gives the `rpc` scope only to
approved applications, and an application that is not approved works for at most 50 invited
testers. Discord's consent screen names the application. And the code Discord hands out is
exchanged for a token by the service, which holds the application's secret, so whoever runs it
sees the tokens.

For an application of your own, register a redirect URI for it, and run a service that takes
`{"code": …}` by POST and answers with Discord's token response (it posts `client_id`,
`client_secret`, `grant_type=authorization_code`, `code` and the registered `redirect_uri` to
`https://discord.com/api/oauth2/token`).

## Building

Needs [Rust](https://rustup.rs), Node.js, [cargo-about](https://github.com/EmbarkStudios/cargo-about)
(`cargo install cargo-about --locked --features cli`) and, on Windows, the WebView2 runtime that
Windows 11 already has.

```bash
cp .env.example .env   # then fill it in: see "The Discord application"
npm install
npm run dev     # run from source
npm run build   # release build and installer, in src-tauri/target/release/bundle
```

On Windows the build makes one installer, an NSIS one
([tauri.windows.conf.json](src-tauri/tauri.windows.conf.json)), in Korean on Korean Windows and
in English everywhere else. It installs for the current user only, into
`%LOCALAPPDATA%\CollaBot Overlay Bridge`, so it needs no administrator rights. It is not
code-signed, so Windows warns before running it. Where CollaBot Overlay is checked out beside the
bridge, in a folder named `collabot-overlay`, `npm run build` then copies the installer into its
`public` folder as `collabot-overlay-bridge-setup.exe`, which the dashboard offers for download
([scripts/copy-installer.mjs](scripts/copy-installer.mjs)).

The Windows installer's license page and the license agreement of the macOS disk image show
[NOTICE.txt](NOTICE.txt), which links the terms of service and the privacy policy and says the
program is under the AGPL. Each of its paragraphs is one line, which the installer wraps to fit.
Every bundle installs [LICENSE](LICENSE) beside the program as `LICENSE.txt`, and
[THIRD-PARTY-LICENSES.txt](THIRD-PARTY-LICENSES.txt), which lists the software by others the
program is built from, with their licenses. `npm run build` first brings that list up to date
with `npm run licenses` ([scripts/licenses.mjs](scripts/licenses.mjs)), which has cargo-about
collect the crates' licenses as [about.toml](src-tauri/about.toml) says, so the file changes when
the crates do. It adds the license of Microsoft's WebView2 loader, which `webview2-com-sys` builds
into the program on Windows but cargo-about cannot see, from
[webview2-loader-license.txt](scripts/webview2-loader-license.txt).

Any run registers the link's scheme for the current user, pointing at the copy that ran last.

### Trying things out

Set when the bridge runs, these environment variables replace parts of the configuration without
a rebuild, what the build was given of the Discord application among them:

| Variable                            | Replaces                                                   |
| ----------------------------------- | ---------------------------------------------------------- |
| `COLLABOT_BRIDGE_ORIGINS`           | Adds sites that may connect, separated by commas           |
| `COLLABOT_BRIDGE_DISCORD_CLIENT_ID` | The Discord application                                    |
| `COLLABOT_BRIDGE_DISCORD_TOKEN_URL` | The service that exchanges codes for tokens                |
| `COLLABOT_BRIDGE_DISCORD_IPC_PATHS` | The pipes to look for Discord at, e.g. `\\.\pipe\stand-in` |
| `COLLABOT_BRIDGE_DATA_DIR`          | The folder the token is kept in                            |

The last three are how the bridge is tested against a stand-in for Discord, without touching a
real account.

## What it keeps

One file, `discord.json` in the app's local data folder (`%LOCALAPPDATA%\bot.colla.overlay.bridge`
on Windows), holding Discord's access token so that the user is asked for permission once. It is
not encrypted. **Discord 권한 지우기** in the window deletes it.

Only Windows has been built and run. Besides what Tauri handles, one part differs by system: the
pipe ([ipc.rs](src-tauri/src/ipc.rs)), a named pipe on Windows and a Unix socket on macOS and
Linux, where it is looked for in the folders Discord's client uses and in those of its Flatpak
and Snap packages. That side has never been compiled.
