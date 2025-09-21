# Neutral P2P Chat

This workspace contains a libp2p rendezvous server and a desktop client (egui/eframe).

Now includes a simple login/register flow backed by a server-side username -> PeerId mapping.

## Components
- server: libp2p rendezvous server with an additional Auth request/response protocol ("/auth/1.0"). Maintains an in-memory map of `username -> PeerId`.
- client: egui desktop app. Shows a Login page on startup to Register/Login a username with the server, then returns to the existing chat UI unchanged.

## Quick start

1) Build everything

```pwsh
cargo build
```

2) Run the rendezvous server (leave it running)

```pwsh
cargo run -p server -- 0.0.0.0:62649
```
It will print its PeerId and listen on the given address. If omitted, the default is 0.0.0.0:62649.

3) Run the client

```pwsh
cargo run -p client -- 127.0.0.1:62649
```
Pass the rendezvous server ip:port the client should dial. If omitted, the default is 127.0.0.1:62649.
- On first launch you'll see a simple Login screen.
- Enter a username and click Register or Login. The server will respond:
  - Register: OK if the name is free; ERR if taken by another PeerId.
  - Login: OK if your current PeerId previously registered that username; ERR otherwise.
- After successful auth, the UI switches to the original chat interface.

4) Discover peers and chat
- The top combo box lists discovered peers (PeerId). Select one to connect.
- Use the bottom text area to send messages.

## Notes
- The server keeps usernames in-memory only. Restarting clears registrations.
- Auth protocol is very simple and not secure; it is intended for demonstration only.
- The rendezvous server address can be overridden via CLI in both binaries:
  - Server: `cargo run -p server -- 0.0.0.0:7000`
  - Client: `cargo run -p client -- 192.168.1.10:7000`
  Defaults are `0.0.0.0:62649` (server) and `127.0.0.1:62649` (client).

## Troubleshooting
- If discovery shows only the server, launch multiple clients; each client will register and discover others.
- Windows firewall may prompt on first run; allow access so peers can listen/dial.
- On Windows, if rebuilding the client fails with a file lock (Access denied) error, close the running client window to unblock the executable, then build again.
