    use async_trait::async_trait;
    use futures::{prelude::*, StreamExt};
    use libp2p::{
        identify, noise, ping, rendezvous, request_response,
        swarm::{NetworkBehaviour, SwarmEvent},
        tcp, yamux, Multiaddr, PeerId,
    };
    use std::{error::Error, io, str::FromStr};
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

        let peer_to_discover: Option<PeerId> = std::env::args()
            .nth(1)
            .and_then(|peer_id_str| PeerId::from_str(&peer_id_str).ok());

        // ---vvv--- BUG FIX: Create a mutable `chat_partner` state ---vvv---
        let mut chat_partner: Option<PeerId> = peer_to_discover;
        // ---^^^--- END OF FIX ---^^^---

        let mut is_registered = false;

        loop {
            tokio::select! {
                // ---vvv--- BUG FIX: Send messages to the `chat_partner` ---vvv---
                Some(line) = rx.recv() => {
                    if let Some(peer_id) = chat_partner {
                        tracing::info!("Sending message: '{}' to peer {}", line, peer_id);
                        swarm.behaviour_mut().request_response.send_request(&peer_id, line);
                    }
                }
                // ---^^^--- END OF FIX ---^^^---
                event = swarm.select_next_some() => {
                    match event {
                        SwarmEvent::NewListenAddr { address, .. } => {
                            tracing::info!("Local node is listening on {}", address);
                            swarm.add_external_address(address);
                        }
                        SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                            tracing::info!("Connected to {} on {:?}", peer_id, endpoint.get_remote_address());
                        }
                        SwarmEvent::ConnectionClosed { peer_id, .. } => {
                            tracing::info!("Disconnected from {}", peer_id);
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

                            if peer_to_discover.is_some() {
                                tracing::info!("Discovering peers in namespace '{}'...", RENDEZVOUS_NAMESPACE.to_string());
                                swarm.behaviour_mut().rendezvous.discover(
                                    Some(rendezvous::Namespace::new(RENDEZVOUS_NAMESPACE.to_string()).unwrap()),
                                    None,
                                    None,
                                    rendezvous_point_peer_id
                                );
                            }
                        }
                        SwarmEvent::Behaviour(ClientBehaviourEvent::Rendezvous(rendezvous::client::Event::Discovered { registrations, .. })) => {
                            if let Some(target_peer_id) = peer_to_discover {
                                for registration in registrations {
                                    let discovered_peer = registration.record.peer_id();
                                    if discovered_peer == target_peer_id {
                                        tracing::info!("Discovered target peer {}", discovered_peer);
                                        for address in registration.record.addresses() {
                                            tracing::info!("Dialing peer {} at address {}", discovered_peer, address);
                                            if let Err(e) = swarm.dial(address.clone()) {
                                                tracing::error!("Failed to dial discovered peer: {}", e);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        SwarmEvent::Behaviour(ClientBehaviourEvent::RequestResponse(event)) => match event {
                            request_response::Event::Message { peer, message } => {
                                match message {
                                    request_response::Message::Request { request, channel, .. } => {
                                        let request_str = request.to_string();
                                        tracing::info!("Received request: '{}' from peer {}", request_str, peer);
                                        println!("{}: {}", peer, request_str);

                                        // ---vvv--- BUG FIX: Set the partner on first message ---vvv---
                                        if chat_partner.is_none() {
                                            tracing::info!("Setting chat partner to {}", peer);
                                            chat_partner = Some(peer);
                                        }
                                        // ---^^^--- END OF FIX ---^^^---

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