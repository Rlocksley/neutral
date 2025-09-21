# Neutral P2P Chat

Peer-to-peer chat built with Rust, libp2p, and an egui/eframe desktop client. A lightweight rendezvous server handles discovery plus a simple username-based auth directory so users can pick who to chat with by name (not by PeerId).

This repo contains two crates:
- server — libp2p rendezvous server with an additional auth request/response protocol ("/auth/1.0"). Maintains a runtime map of `username -> PeerId` for online users and a simple user database for registration/login.
- client — desktop app using egui. Shows a Login/Register screen, then a chat UI with a username dropdown sourced from the server.

## Features
- Username-based directory for selecting peers (self is omitted)
- Stable, case-insensitive alphabetical ordering of usernames
- Live updates: the client refreshes the directory periodically (defaults ~5s)
- Cleanup on disconnect: server removes usernames when clients go offline; clients also send an explicit LOGOUT on close (best effort)
- Configurable rendezvous address via CLI for both server and client

## Architecture at a glance
- Transport/protocols: libp2p with TCP, Noise, Yamux, Identify, Ping, Rendezvous, and Request/Response
- Chat protocol: simple request/response exchanging text messages ("/hello/1.0")
- Auth protocol ("/auth/1.0"): plaintext control messages
  - REGISTER:<username>|<password>|<yyyy-mm-dd>
  - LOGIN:<username>|<password>
  - LIST → returns `LIST:userA=PeerIdA,userB=PeerIdB,...`
  - LOGOUT:<username>
- User database: stored on the server (see `server/users.xml`). Passwords are stored as a SHA-256 hash (demo only; no salt).
- Online directory: in-memory `username -> PeerId` map updated on login/logout and when connections close.

## Build

```pwsh
cargo build
```

## Run

1) Start the rendezvous/auth server (leave it running):

```pwsh
cargo run -p server -- 0.0.0.0:62649
```

Notes:
- The server listens on the provided `ip:port`. If omitted, it defaults to `0.0.0.0:62649`.
- On startup it prints its `PeerId` and bound address.

2) Start one or more clients (each in its own terminal):

```pwsh
cargo run -p client -- 127.0.0.1:62649
```

Notes:
- The client dials the rendezvous server at the given `ip:port`. Default: `127.0.0.1:62649`.
- First screen is Login/Register. After successful auth you’ll see the chat UI.

## Using the app
1) Register or Login
- Register succeeds if the username is free; otherwise you’ll see an error.
- Login succeeds only if your current PeerId previously registered that username.

2) Pick a user to chat with
- The top bar shows a “User” dropdown listing online usernames (excluding yourself).
- The list is sorted alphabetically (case-insensitive) and refreshes every few seconds.
- Selecting a user will automatically connect to that peer.

3) Chat
- Type in the bottom input and click Send. Messages appear right-aligned for you (prefixed "You to ...") and left-aligned for incoming messages.

## CLI reference
- Server: `cargo run -p server -- [ip:port]`
  - Default: `0.0.0.0:62649`
- Client: `cargo run -p client -- [ip:port]`
  - Default: `127.0.0.1:62649`

## Troubleshooting
- Windows: "Access is denied (os error 5)" when building — a running `server.exe` or `client.exe` is locking the file. Close the app(s) and build again.
- Windows firewall may prompt on first run. Allow access so peers can listen/dial.
- Don’t see new users immediately? The username list is refreshed periodically (about every 5s). Wait a moment or restart the client if needed.
- If the rendezvous server goes offline, clients will clear the user list and repopulate on reconnect.

## Notes and limitations
- Demo-grade auth: passwords are hashed with SHA-256 and stored in `users.xml` without salt. Do not use this as-is for production.
- The runtime `username -> PeerId` directory is not persisted. It’s rebuilt from client sessions and cleared on server restart; the user database remains.
- Chat messages are plaintext over the simple request/response protocol (suitable for demos only).

## License

See the LICENSE file in this repository.
