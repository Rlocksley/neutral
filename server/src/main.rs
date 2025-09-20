    use async_trait::async_trait;
    use futures::{prelude::*, StreamExt};
    use libp2p::{
        identify, noise, ping, rendezvous, request_response,
        swarm::{NetworkBehaviour, SwarmEvent},
        tcp, yamux,
    };
    use std::{error::Error, io};
    use tracing_subscriber::EnvFilter;

    // --- Protocol Definition ---
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
            // THE FIX: Pass a new mutable reference to the function.
            let vec = unsigned_varint::aio::read_u16(&mut *io)
                .await
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let mut buffer = vec![0; vec as usize];
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
            // THE FIX: Pass a new mutable reference to the function.
            let vec = unsigned_varint::aio::read_u16(&mut *io)
                .await
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let mut buffer = vec![0; vec as usize];
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

    // --- Main Application Logic ---       
    #[tokio::main]
    async fn main() -> Result<(), Box<dyn Error>> {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
            )
            .try_init();

        let keypair = libp2p::identity::Keypair::ed25519_from_bytes([0; 32]).unwrap();

        let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )?
            .with_behaviour(|key| MyBehaviour {
                identify: identify::Behaviour::new(identify::Config::new(
                    "rendezvous-example/1.0.0".to_string(),
                    key.public(),
                )),
                rendezvous: rendezvous::server::Behaviour::new(rendezvous::server::Config::default()),
                ping: ping::Behaviour::new(ping::Config::default()),
                request_response: request_response::Behaviour::new(
                    std::iter::once((HelloProtocol(), request_response::ProtocolSupport::Full)),
                    request_response::Config::default(),
                ),
            })?
             .with_swarm_config(|c: libp2p::swarm::Config| c.with_idle_connection_timeout(std::time::Duration::from_secs(2)))
            .build();

        let _ = swarm.listen_on("/ip4/0.0.0.0/tcp/62649".parse().unwrap());

        while let Some(event) = swarm.next().await {
            match event {
                SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                    tracing::info!("Connected to {}", peer_id);
                }
                SwarmEvent::ConnectionClosed { peer_id, .. } => {
                    tracing::info!("Disconnected from {}", peer_id);
                }
                SwarmEvent::Behaviour(MyBehaviourEvent::Rendezvous(
                    rendezvous::server::Event::PeerRegistered { peer, registration },
                )) => {
                    tracing::info!(
                        "Peer {} registered for namespace '{}'",
                        peer,
                        registration.namespace
                    );
                }
                SwarmEvent::Behaviour(MyBehaviourEvent::Rendezvous(
                    rendezvous::server::Event::DiscoverServed {
                        enquirer,
                        registrations,
                    },
                )) => {
                    tracing::info!(
                        "Served peer {} with {} registrations",
                        enquirer,
                        registrations.len()
                    );
                }
                SwarmEvent::Behaviour(MyBehaviourEvent::RequestResponse(event)) => match event {
                    request_response::Event::Message { peer, message } => match message {
                        request_response::Message::Request {
                            request, channel, ..
                        } => {
                            tracing::info!("Received request: '{}' from peer {}", request, peer);
                            if let Err(e) = swarm.behaviour_mut().request_response.send_response(
                                channel,
                                "Hello Back from Server".to_string(),
                            ) {
                                tracing::error!("Failed to send response: {}", e);
                            }
                        }
                        request_response::Message::Response { response, .. } => {
                            tracing::warn!(
                                "Received unexpected response: '{}' from peer {}",
                                response,
                                peer
                            );
                        }
                    },
                    _ => {}
                },
                other => {
                    tracing::debug!("Unhandled {:?}", other);
                }
            }
        }

        Ok(())
    }

    // --- Network Behaviour Definition ---
    #[derive(NetworkBehaviour)]
    struct MyBehaviour {
        identify: identify::Behaviour,
        rendezvous: rendezvous::server::Behaviour,
        ping: ping::Behaviour,
        request_response: request_response::Behaviour<HelloCodec>,
    }