    use async_trait::async_trait;
    use futures::{prelude::*, StreamExt};
    use libp2p::{
        identify, noise, ping, rendezvous, request_response,
        swarm::{NetworkBehaviour, SwarmEvent},
        tcp, yamux, Multiaddr, PeerId,
    };
    use std::{collections::{HashMap, HashSet}, error::Error, io, str::FromStr};
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::sync::mpsc;
    use tracing_subscriber::EnvFilter;

    // --- Protocol Definition (Identical to Server) ---
    const RENDEZVOUS_NAMESPACE: &str = "p2p-client";

    #[derive(Debug, Clone)]
    struct HelloProtocol();

    #[derive(Default, Clone)]
    struct HelloCodec();

    impl AsRef<str> for HelloProtocol {
        fn as_ref(&self) -> &str {
            "/hello/1.0"
        }
    }

    #[async_trait]
    impl request_response::Codec for HelloCodec {
        type Protocol = HelloProtocol;
        type Request = String;
        type Response = String;

        async fn read_request<T>(&mut self, _: &Self::Protocol, io: &mut T) -> io::Result<Self::Request>
        where
            T: AsyncRead + Unpin + Send,
        {
            let len = unsigned_varint::aio::read_u16(&mut *io)
                .await
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let mut buffer = vec![0; len as usize];
            io.read_exact(&mut buffer).await?;
            Ok(String::from_utf8(buffer).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?)
        }

        async fn read_response<T>(
            &mut self,
            _: &Self::Protocol,
            io: &mut T,
        ) -> io::Result<Self::Response>
        where
            T: AsyncRead + Unpin + Send,
        {
            let len = unsigned_varint::aio::read_u16(&mut *io)
                .await
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let mut buffer = vec![0; len as usize];
            io.read_exact(&mut buffer).await?;
            Ok(String::from_utf8(buffer).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?)
        }

        async fn write_request<T>(
            &mut self,
            _: &Self::Protocol,
            io: &mut T,
            req: Self::Request,
        ) -> io::Result<()>
        where
            T: AsyncWrite + Unpin + Send,
        {
            let mut uvi_buf = unsigned_varint::encode::u16_buffer();
            let encoded_len = unsigned_varint::encode::u16(req.len() as u16, &mut uvi_buf);

            io.write_all(encoded_len).await?;
            io.write_all(req.as_bytes()).await?;
            io.flush().await
        }

        async fn write_response<T>(
            &mut self,
            _: &Self::Protocol,
            io: &mut T,
            res: Self::Response,
        ) -> io::Result<()>
        where
            T: AsyncWrite + Unpin + Send,
        {
            let mut uvi_buf = unsigned_varint::encode::u16_buffer();
            let encoded_len = unsigned_varint::encode::u16(res.len() as u16, &mut uvi_buf);

            io.write_all(encoded_len).await?;
            io.write_all(res.as_bytes()).await?;
            io.flush().await
        }
    }
    #[derive(Debug)]
    enum UserCmd {
        Connect { peer: PeerId },
        Send { peer: PeerId, msg: String },
        Disconnect { peer: PeerId },
        Help,
        Unknown(String),
    }

    fn parse_user_cmd(line: &str) -> UserCmd {
        // Supported commands:
        // connect -pId <peerId>
        // write -pId <peerId> -m <message>
        // disconnect -pId <peerId>
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            return UserCmd::Unknown(String::new());
        }

        // Normalize flags to lowercase for robustness
        let lower = parts.iter().map(|s| s.to_ascii_lowercase()).collect::<Vec<_>>();

        // Helper to get value after a flag
        let get_after = |flag: &str| -> Option<String> {
            for i in 0..lower.len() {
                if lower[i] == flag && i + 1 < parts.len() {
                    return Some(parts[i + 1].to_string());
                }
            }
            None
        };

        // connect -pId <peer>
        if lower[0] == "connect" {
            if let Some(pid_str) = get_after("-pid") {
                if let Ok(peer) = PeerId::from_str(&pid_str) {
                    return UserCmd::Connect { peer };
                }
            }
            return UserCmd::Unknown(line.to_string());
        }

        // disconnect -pId <peer>
        if lower[0] == "disconnect" {
            if let Some(pid_str) = get_after("-pid") {
                if let Ok(peer) = PeerId::from_str(&pid_str) {
                    return UserCmd::Disconnect { peer };
                }
            }
            return UserCmd::Unknown(line.to_string());
        }

        // write -pId <peer> -m <message...>
        if lower[0] == "write" {
            if let Some(pid_str) = get_after("-pid") {
                if let Ok(peer) = PeerId::from_str(&pid_str) {
                    // message may contain spaces; find index of -m and join rest
                    let mut msg = String::new();
                    for i in 0..lower.len() {
                        if lower[i] == "-m" && i + 1 < parts.len() {
                            msg = parts[i + 1..].join(" ");
                            break;
                        }
                    }
                    if !msg.is_empty() {
                        return UserCmd::Send { peer, msg };
                    }
                }
            }
            return UserCmd::Unknown(line.to_string());
        }

        if lower[0] == "-help" || lower[0] == "--help" || lower[0] == "help" {
            return UserCmd::Help;
        }

        UserCmd::Unknown(line.to_string())
    }

    #[tokio::main]
    async fn main() -> Result<(), Box<dyn Error>> {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
            )
            .init();
        let local_key = libp2p::identity::Keypair::generate_ed25519();
        let local_peer_id = PeerId::from(local_key.public());
        tracing::info!("Local peer id: {}", local_peer_id);
        println!("Local peer id: {}", local_peer_id);

        let (tx, mut rx) = mpsc::channel(10);

        tokio::spawn(async move {
            let mut stdin = BufReader::new(tokio::io::stdin()).lines();
            loop {
                match stdin.next_line().await {
                    Ok(Some(line)) => {
                        if tx.send(line).await.is_err() {
                            break;
                        }
                    }
                    _ => {
                        break;
                    }
                }
            }
        });

        let mut swarm = libp2p::SwarmBuilder::with_existing_identity(local_key)
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )?
            .with_behaviour(|key| ClientBehaviour {
                rendezvous: rendezvous::client::Behaviour::new(key.clone()),
                ping: ping::Behaviour::new(ping::Config::default()),
                identify: identify::Behaviour::new(identify::Config::new(
                    "/p2p-client/1.0.0".to_string(),
                    key.public(),
                )),
                request_response: request_response::Behaviour::new(
                    std::iter::once((HelloProtocol(), request_response::ProtocolSupport::Full)),
                    request_response::Config::default(),
                ),
            })?
            .with_swarm_config(|c: libp2p::swarm::Config| c.with_idle_connection_timeout(std::time::Duration::from_secs(60)))
            .build();

        swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

        let rendezvous_point_address: Multiaddr = "/ip4/127.0.0.1/tcp/62649".parse()?;
        let rendezvous_point_peer_id =
            PeerId::from_str("12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN")?;

        swarm
            .dial(rendezvous_point_address.clone())
            .expect("Failed to dial rendezvous server");

        // Maintain discovered peers and their multiaddrs and a set of connected peers
        let mut discovered: HashMap<PeerId, Vec<Multiaddr>> = HashMap::new();
        let mut connected: HashSet<PeerId> = HashSet::new();

        let mut is_registered = false;

    println!("Type connect -pId <PeerId> to connect, write -pId <PeerId> -m <Message> to send, disconnect -pId <PeerId> to disconnect. Type -help for help.");

        loop {
            tokio::select! {
                Some(line) = rx.recv() => {
                    match parse_user_cmd(&line) {
                        UserCmd::Connect { peer } => {
                            if peer == rendezvous_point_peer_id {
                                println!("Cannot connect chat to rendezvous server peer id.");
                                continue;
                            }
                            if let Some(addrs) = discovered.get(&peer) {
                                for addr in addrs {
                                    tracing::info!("Dialing {} at {}", peer, addr);
                                    if let Err(e) = swarm.dial(addr.clone()) {
                                        tracing::error!("Dial to {} at {} failed: {}", peer, addr, e);
                                    }
                                }
                            } else {
                                println!("Peer not in discovered list. Try again after discovery or ensure peer is running.");
                            }
                        }
                        UserCmd::Send { peer, msg } => {
                            if !connected.contains(&peer) {
                                // Try dialing if we know an address
                                if let Some(addrs) = discovered.get(&peer) {
                                    for addr in addrs {
                                        let _ = swarm.dial(addr.clone());
                                    }
                                }
                            }
                            tracing::info!("Sending message: '{}' to peer {}", msg, peer);
                            swarm.behaviour_mut().request_response.send_request(&peer, msg);
                        }
                        UserCmd::Disconnect { peer } => {
                            if let Err(e) = swarm.disconnect_peer_id(peer) {
                                tracing::warn!("Disconnect request for {} failed: {:?}", peer, e);
                            }
                        }
                        UserCmd::Help => {
                            println!("Commands:\n  connect -pId <PeerId>\n  write -pId <PeerId> -m <Message>\n  disconnect -pId <PeerId>");
                        }
                        UserCmd::Unknown(_) => {
                            println!("Unrecognized input. Type -help for usage.");
                        }
                    }
                }
                event = swarm.select_next_some() => {
                    match event {
                        SwarmEvent::NewListenAddr { address, .. } => {
                            tracing::info!("Local node is listening on {}", address);
                            swarm.add_external_address(address);
                        }
                        SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                            tracing::info!("Connected to {} on {:?}", peer_id, endpoint.get_remote_address());
                            connected.insert(peer_id);
                        }
                        SwarmEvent::ConnectionClosed { peer_id, .. } => {
                            tracing::info!("Disconnected from {}", peer_id);
                            connected.remove(&peer_id);
                        }
                        SwarmEvent::Behaviour(ClientBehaviourEvent::Identify(identify::Event::Received { peer_id, info, })) => {
                            tracing::info!("Received identify info from {}: observed address {:?}", peer_id, info.observed_addr);

                            if peer_id == rendezvous_point_peer_id && !is_registered {
                                tracing::info!("Identity protocol finished with rendezvous server. Registering...");
                                if let Err(e) = swarm.behaviour_mut().rendezvous.register(
                                    rendezvous::Namespace::new(RENDEZVOUS_NAMESPACE.to_string()).unwrap(),
                                    rendezvous_point_peer_id,
                                    None,
                                ) {
                                    tracing::error!("Failed to send registration request: {:?}", e);
                                }
                            }
                        }
                        SwarmEvent::Behaviour(ClientBehaviourEvent::Rendezvous(rendezvous::client::Event::Registered { .. })) => {
                            tracing::info!("Successfully registered with the rendezvous server!");
                            is_registered = true;
                            // Always discover peers after registering
                            tracing::info!("Discovering peers in namespace '{}'...", RENDEZVOUS_NAMESPACE.to_string());
                            swarm.behaviour_mut().rendezvous.discover(
                                Some(rendezvous::Namespace::new(RENDEZVOUS_NAMESPACE.to_string()).unwrap()),
                                None,
                                None,
                                rendezvous_point_peer_id
                            );
                        }
                        SwarmEvent::Behaviour(ClientBehaviourEvent::Rendezvous(rendezvous::client::Event::Discovered { registrations, .. })) => {
                            // Update discovered peers map and print available peers
                            for registration in registrations {
                                let discovered_peer = registration.record.peer_id();
                                if discovered_peer == local_peer_id { continue; }
                                let entry = discovered.entry(discovered_peer).or_default();
                                for address in registration.record.addresses() {
                                    if !entry.contains(address) {
                                        entry.push(address.clone());
                                    }
                                }
                            }
                            if discovered.is_empty() {
                                println!("No peers discovered yet.");
                            } else {
                                println!("Discovered peers:");
                                for peer in discovered.keys() {
                                    println!("  {}", peer);
                                }
                            }
                        }
                        SwarmEvent::Behaviour(ClientBehaviourEvent::RequestResponse(event)) => match event {
                            request_response::Event::Message { peer, message } => {
                                match message {
                                    request_response::Message::Request { request, channel, .. } => {
                                        let request_str = request.to_string();
                                        tracing::info!("Received request: '{}' from peer {}", request_str, peer);
                                        // Print in requested format
                                        println!(".\n.\n.\n{}\n{}\n.\n.\n.", peer, request_str);

                                        if let Err(e) = swarm.behaviour_mut().request_response.send_response(channel, ".".to_string()) {
                                            tracing::error!("Failed to send response: {}", e);
                                        }
                                    }
                                    request_response::Message::Response { response, .. } => {
                                        tracing::info!("Received response: '{}' from peer {}", response.to_string(), peer);
                                    }
                                }
                            }
                            request_response::Event::OutboundFailure { peer, .. } => {
                                tracing::error!("Outbound request to {} failed", peer);
                            }
                            _ => {}
                        },
                        other => {
                            tracing::debug!("Unhandled swarm event: {:?}", other);
                        }
                    }
                }
            }
        }
    }
    // --- Network Behaviour Definition ---
    #[derive(NetworkBehaviour)]
    struct ClientBehaviour {
        rendezvous: rendezvous::client::Behaviour,
        ping: ping::Behaviour,
        identify: identify::Behaviour,
        request_response: request_response::Behaviour<HelloCodec>,
    }