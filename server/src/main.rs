use async_trait::async_trait;
use futures::{prelude::*, StreamExt};
use libp2p::{
    identify, noise, ping, rendezvous, request_response,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux,
    PeerId,
};
use std::{error::Error, io, collections::HashMap, fs, path::Path};
use tracing_subscriber::EnvFilter;
use serde::{Serialize, Deserialize};
use sha2::{Sha256, Digest};

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

// --- Auth Protocol Definition ---
#[derive(Debug, Clone)]
struct AuthProtocol();

#[derive(Default, Clone)]
struct AuthCodec();

impl AsRef<str> for AuthProtocol {
    fn as_ref(&self) -> &str {
        "/auth/1.0"
    }
}

#[async_trait]
impl request_response::Codec for AuthCodec {
    type Protocol = AuthProtocol;
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

// --- Main Application Logic ---       
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .try_init();

    // Optional CLI: ip:port to listen on (defaults to 0.0.0.0:62649)
    let listen_arg = std::env::args().nth(1).unwrap_or_else(|| "0.0.0.0:62649".to_string());
    let (listen_ip, listen_port) = match listen_arg.split_once(':') {
        Some((ip, port)) if !ip.is_empty() && !port.is_empty() => (ip.to_string(), port.to_string()),
        _ => ("0.0.0.0".to_string(), "62649".to_string()),
    };

    let keypair = libp2p::identity::Keypair::ed25519_from_bytes([0; 32]).unwrap();
    let server_peer_id = libp2p::PeerId::from(keypair.public());
    println!("Rendezvous server peer id: {}", server_peer_id);

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
            auth: request_response::Behaviour::new(
                std::iter::once((AuthProtocol(), request_response::ProtocolSupport::Full)),
                request_response::Config::default(),
            ),
        })?
        .with_swarm_config(|c: libp2p::swarm::Config| c.with_idle_connection_timeout(std::time::Duration::from_secs(60)))
        .build();

    let listen_multiaddr_str = format!("/ip4/{}/tcp/{}", listen_ip, listen_port);
    let _ = swarm.listen_on(listen_multiaddr_str.parse().unwrap());
    println!("Listening on {}", listen_multiaddr_str);

    // Persistent user store
    let users_path = Path::new("users.xml");
    let mut users_xml = load_users(users_path);
    let mut users_by_name: HashMap<String, (String, String)> = HashMap::new();
    for u in &users_xml.users {
        users_by_name.insert(u.username.clone(), (u.password_hash.clone(), u.birthdate.clone()));
    }
    let mut username_to_peer: HashMap<String, PeerId> = HashMap::new();

    while let Some(event) = swarm.next().await {
        match event {
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                tracing::info!("Connected to {}", peer_id);
            }
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                tracing::info!("Disconnected from {}", peer_id);
                // Remove any usernames associated with this peer so LIST stays accurate
                let mut removed: Vec<String> = Vec::new();
                username_to_peer.retain(|name, pid| {
                    let keep = *pid != peer_id;
                    if !keep { removed.push(name.clone()); }
                    keep
                });
                if !removed.is_empty() {
                    tracing::info!("Removed usernames on disconnect: {:?}", removed);
                }
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
            // Chat protocol
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
            // Auth protocol
            SwarmEvent::Behaviour(MyBehaviourEvent::Auth(event)) => match event {
                request_response::Event::Message { peer, message } => match message {
                    request_response::Message::Request { request, channel, .. } => {
                        let text = request.to_string();
                        // Expect formats:
                        // REGISTER:username|password|YYYY-MM-DD
                        // LOGIN:username|password
                        let resp = if let Some(rest) = text.strip_prefix("REGISTER:") {
                            let parts: Vec<&str> = rest.split('|').collect();
                            if parts.len() != 3 { "ERR:Invalid register payload".to_string() }
                            else {
                                let name = parts[0].trim().to_string();
                                let pw = parts[1];
                                let dob = parts[2].trim().to_string();
                                match users_by_name.get(&name) {
                                    None => {
                                        let pw_hash = hash_password(pw);
                                        users_by_name.insert(name.clone(), (pw_hash.clone(), dob.clone()));
                                        users_xml.users.push(UserXml { username: name.clone(), password_hash: pw_hash, birthdate: dob, peer_id: None });
                                        save_users(users_path, &users_xml);
                                        username_to_peer.insert(name, peer);
                                        "AUTH:OK".to_string()
                                    }
                                    Some(_) => "AUTH:ERR:Username taken".to_string(),
                                }
                            }
                        } else if let Some(rest) = text.strip_prefix("LOGIN:") {
                            let parts: Vec<&str> = rest.split('|').collect();
                            if parts.len() != 2 { "ERR:Invalid login payload".to_string() }
                            else {
                                let name = parts[0].trim();
                                let pw = parts[1];
                                match users_by_name.get(name) {
                                    Some((hash, _dob)) => {
                                        if *hash == hash_password(pw) {
                                            match username_to_peer.get(name) {
                                                Some(pid) if *pid == peer => "AUTH:OK".to_string(),
                                                Some(_) => "AUTH:ERR:Username belongs to another peer".to_string(),
                                                None => { username_to_peer.insert(name.to_string(), peer); "AUTH:OK".to_string() }
                                            }
                                        } else {
                                            "AUTH:ERR:Invalid password".to_string()
                                        }
                                    }
                                    None => "AUTH:ERR:Unknown user".to_string(),
                                }
                            }
                        } else if let Some(rest) = text.strip_prefix("LOGOUT:") {
                            let name = rest.trim();
                            match username_to_peer.get(name) {
                                Some(pid) if *pid == peer => {
                                    username_to_peer.remove(name);
                                    "AUTH:OK".to_string()
                                }
                                Some(_) => "AUTH:ERR:Username belongs to another peer".to_string(),
                                None => "AUTH:ERR:Unknown user".to_string(),
                            }
                        } else if text.trim() == "LIST" {
                            // Return a mapping of username=peerid for all logged-in users
                            let mut pairs: Vec<String> = Vec::new();
                            for (name, pid) in &username_to_peer {
                                pairs.push(format!("{}={}", name, pid));
                            }
                            format!("LIST:{}", pairs.join(","))
                        } else {
                            "AUTH:ERR:Unknown command".to_string()
                        };
                        if let Err(e) = swarm.behaviour_mut().auth.send_response(channel, resp) {
                            tracing::error!("Failed to send auth response: {}", e);
                        }
                    }
                    _ => {}
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
    auth: request_response::Behaviour<AuthCodec>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct UsersXml {
    #[serde(rename = "user", default)]
    users: Vec<UserXml>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct UserXml {
    #[serde(rename = "username")]
    username: String,
    #[serde(rename = "password_hash")]
    password_hash: String,
    #[serde(rename = "birthdate")]
    birthdate: String, // YYYY-MM-DD
    #[serde(skip)]
    #[serde(default)]
    peer_id: Option<PeerId>,
}

fn hash_password(pw: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(pw.as_bytes());
    let out = hasher.finalize();
    hex::encode(out)
}

fn load_users(path: &Path) -> UsersXml {
    if let Ok(text) = fs::read_to_string(path) {
        quick_xml::de::from_str::<UsersXml>(&text).unwrap_or_default()
    } else {
        UsersXml::default()
    }
}

fn save_users(path: &Path, users: &UsersXml) {
    if let Ok(xml) = quick_xml::se::to_string(users) {
        // Store with a simple root header
        let xml_wrapped = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<users>\n{}\n</users>", xml);
        let _ = fs::write(path, xml_wrapped);
    }
}