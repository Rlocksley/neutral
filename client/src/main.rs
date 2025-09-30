use async_trait::async_trait;
use futures::{prelude::*, StreamExt};
use libp2p::{
    identify, noise, ping, rendezvous, request_response,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr, PeerId,
};
use std::{collections::{HashMap, HashSet}, io, str::FromStr, time::SystemTime};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tracing_subscriber::EnvFilter;
use eframe::egui;

    // ---- UI Theme & Sizing ------------------------------------------------------
    const UI_HEIGHT: f32 = 36.0; // uniform height for interactive controls
    const BUTTON_WIDTH: f32 = 120.0; // default button width
    const RADIUS: f32 = 8.0; // rounded corners

    fn configure_theme(ctx: &egui::Context) {
        let blue = egui::Color32::from_rgb(25, 118, 210); // #1976D2
        let blue_hover = egui::Color32::from_rgb(30, 136, 229); // #1E88E5
        let blue_dark = egui::Color32::from_rgb(21, 101, 192); // #1565C0
        let orange = egui::Color32::from_rgb(255, 152, 0); // #FF9800
        let orange_dark = egui::Color32::from_rgb(230, 130, 0);

        let mut style = egui::Style::default();
        style.visuals = egui::Visuals::dark();

        // Spacing & element sizing
        style.spacing.interact_size = egui::vec2(0.0, UI_HEIGHT); // enforce uniform height
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 8.0);
        style.spacing.combo_width = 220.0;

        // Global rounding
        let rounding = egui::Rounding::same(RADIUS);
        style.visuals.widgets.noninteractive.rounding = rounding;
        style.visuals.widgets.inactive.rounding = rounding;
        style.visuals.widgets.hovered.rounding = rounding;
        style.visuals.widgets.active.rounding = rounding;
        style.visuals.widgets.open.rounding = rounding;

        // Accents & selections
        style.visuals.selection.bg_fill = blue;
        style.visuals.selection.stroke = egui::Stroke { width: 1.0, color: orange };
        style.visuals.hyperlink_color = blue;

        // Button-esque widget visuals
        style.visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(45, 49, 55);
        style.visuals.widgets.inactive.fg_stroke = egui::Stroke { width: 1.0, color: egui::Color32::LIGHT_GRAY };
        style.visuals.widgets.hovered.bg_fill = blue_hover;
        style.visuals.widgets.hovered.fg_stroke = egui::Stroke { width: 1.0, color: egui::Color32::WHITE };
        style.visuals.widgets.hovered.bg_stroke = egui::Stroke { width: 1.0, color: blue_dark };
        style.visuals.widgets.active.bg_fill = orange;
        style.visuals.widgets.active.fg_stroke = egui::Stroke { width: 1.0, color: egui::Color32::WHITE };
        style.visuals.widgets.active.bg_stroke = egui::Stroke { width: 1.0, color: orange_dark };

        // Panels / backgrounds
        style.visuals.panel_fill = egui::Color32::from_rgb(24, 27, 31);
        style.visuals.window_fill = egui::Color32::from_rgb(22, 24, 28);
        style.visuals.window_stroke = egui::Stroke { width: 1.0, color: egui::Color32::from_rgb(40, 44, 50) };

        ctx.set_style(style);
    }

    // --- Protocol Definition (must match the server) -----------------------------
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

    // --- Auth Protocol -----------------------------------------------------------
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

    // Messages from UI to networking task
    #[derive(Debug, Clone)]
    enum UiToNet {
        Connect { peer_id: String },
        Write { peer_id: String, from_username: String, to_username: String, msg: String },
        Register { username: String, password: String, birthdate: String },
        Login { username: String, password: String },
        Logout { username: String },
        DeleteAccount { username: String, password: String },
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum MessageDirection {
        Incoming,
        Outgoing,
    }

    // Messages from networking task to UI
    #[derive(Debug, Clone)]
    enum NetToUi {
        Discovered(Vec<String>),
        Connected(String),
        Disconnected(String),
        ChatMessage { peer: String, direction: MessageDirection, text: String },
        Info(String),
        Error(String),
        AuthResult { ok: bool, message: String },
        Users(HashMap<String, String>), // username -> PeerId
        DeleteResult { ok: bool, message: String },
    }

    fn main() -> eframe::Result<()> {
        // Setup logging
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
            )
            .try_init();

    // Optional CLI: rendezvous server ip:port (defaults to 127.0.0.1:62649)
    let rendezvous_arg = std::env::args().nth(1).unwrap_or_else(|| "127.0.0.1:62649".to_string());
    let (rv_ip, rv_port) = match rendezvous_arg.split_once(':') {
        Some((ip, port)) if !ip.is_empty() && !port.is_empty() => (ip.to_string(), port.to_string()),
        _ => ("127.0.0.1".to_string(), "62649".to_string()),
    };
    let rendezvous_multiaddr: Multiaddr = format!("/ip4/{}/tcp/{}", rv_ip, rv_port)
        .parse()
        .unwrap_or_else(|_| "/ip4/127.0.0.1/tcp/62649".parse().unwrap());

    // Build a Tokio runtime for networking and keep it alive for app lifetime
    let rt = std::sync::Arc::new(tokio::runtime::Runtime::new().expect("Tokio runtime"));

        // Create channels between UI and networking task
        let (ui_to_net_tx, ui_to_net_rx) = tokio::sync::mpsc::unbounded_channel::<UiToNet>();
        let (net_to_ui_tx, net_to_ui_rx) = tokio::sync::mpsc::unbounded_channel::<NetToUi>();

    // Spawn networking task
    rt.spawn(network_task(ui_to_net_rx, net_to_ui_tx, rendezvous_multiaddr.clone()));

        // Keep runtime alive by holding it in scope while UI runs
        let native_options = eframe::NativeOptions::default();
        eframe::run_native(
            "P2P Chat Client",
            native_options,
            Box::new(|cc| {
                // Apply our theme before UI starts
                configure_theme(&cc.egui_ctx);
                Box::new(ChatApp::new(ui_to_net_tx, net_to_ui_rx, rt))
            }),
        )
    }

    // The eframe/egui application struct
    #[derive(Debug, Clone)]
    struct ChatMessage {
        from_self: bool,
        text: String,
    }

    #[derive(Debug, Clone)]
    struct Conversation {
        messages: Vec<ChatMessage>,
        unread: bool,
        last_activity: SystemTime,
    }

    impl Default for Conversation {
        fn default() -> Self {
            Self {
                messages: Vec::new(),
                unread: false,
                last_activity: SystemTime::UNIX_EPOCH,
            }
        }
    }

    struct ChatApp {
        tx: UnboundedSender<UiToNet>,
        rx: UnboundedReceiver<NetToUi>,
        // Hold the runtime to keep it alive for as long as the UI runs
        _rt: std::sync::Arc<tokio::runtime::Runtime>,
    conversations: HashMap<String, Conversation>,
        users: HashMap<String, String>, // username -> PeerId
        selected_user: Option<String>,
        peer_to_username: HashMap<String, String>, // PeerId -> username (for labeling incoming)
        message_input: String,
        status: String,
        // Login state
        logged_in: bool,
        username: String,
        username_input: String,
        password_input: String,
        auth_feedback: String,
        // Register page state
        page: Page,
        reg_username: String,
        reg_password: String,
        // Birthdate parts for a structured chooser
        reg_birth_year: i32,
        reg_birth_month: u32, // 1-12
        reg_birth_day: u32,   // 1..=days_in_month
        // Delete account view
        show_delete_view: bool,
        del_username: String,
        del_password: String,
        del_feedback: String,
    }

    // UI pages
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Page { Login, Register }

    impl ChatApp {
        fn new(tx: UnboundedSender<UiToNet>, rx: UnboundedReceiver<NetToUi>, rt: std::sync::Arc<tokio::runtime::Runtime>) -> Self {
            Self {
                tx, rx, _rt: rt,
                conversations: HashMap::new(),
                users: HashMap::new(), selected_user: None, peer_to_username: HashMap::new(),
                message_input: String::new(),
                status: String::from("Please login or register"), logged_in: false,
                
                username: String::new(), username_input: String::new(), password_input: String::new(),
                auth_feedback: String::new(),
                page: Page::Login,
                reg_username: String::new(), reg_password: String::new(),
                // Sensible defaults
                reg_birth_year: 2000,
                reg_birth_month: 1,
                reg_birth_day: 1,
                show_delete_view: false,
                del_username: String::new(),
                del_password: String::new(),
                del_feedback: String::new(),
            }
        }
    }

    impl eframe::App for ChatApp {
        fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            // Ensure regular repaint so incoming messages are processed promptly
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
            // Drain messages from networking
            while let Ok(msg) = self.rx.try_recv() {
                match msg {
                    NetToUi::Discovered(list) => {
                        self.status = format!("Discovered {} peer(s)", list.len());
                        ctx.request_repaint();
                    }
                    NetToUi::Connected(pid) => {
                        // Do not expose peer IDs. Prefer username mapping if available.
                        let label = self
                            .peer_to_username
                            .get(&pid)
                            .cloned()
                            .or_else(|| {
                                // Fallback: try reverse lookup from users map
                                self.users
                                    .iter()
                                    .find_map(|(uname, upid)| if upid == &pid { Some(uname.clone()) } else { None })
                            });
                        self.status = match label {
                            Some(name) => format!("Connected to {}", name),
                            None => "Connected".to_string(),
                        };
                        ctx.request_repaint();
                    }
                    NetToUi::Disconnected(pid) => {
                        // Do not expose peer IDs.
                        let label = self
                            .peer_to_username
                            .get(&pid)
                            .cloned()
                            .or_else(|| {
                                self.users
                                    .iter()
                                    .find_map(|(uname, upid)| if upid == &pid { Some(uname.clone()) } else { None })
                            });
                        self.status = match label {
                            Some(name) => format!("Disconnected from {}", name),
                            None => "Disconnected".to_string(),
                        };
                        ctx.request_repaint();
                    }
                    NetToUi::ChatMessage { peer, direction, text } => {
                        let entry = self.conversations.entry(peer.clone()).or_default();
                        let from_self = matches!(direction, MessageDirection::Outgoing);
                        entry.messages.push(ChatMessage { from_self, text });
                        entry.last_activity = SystemTime::now();
                        if from_self || self.selected_user.as_ref() == Some(&peer) {
                            entry.unread = false;
                        } else {
                            entry.unread = true;
                        }
                        ctx.request_repaint();
                    }
                    NetToUi::Info(s) => self.status = s,
                    NetToUi::Error(e) => self.status = format!("Error: {}", e),
                    NetToUi::AuthResult { ok, message } => {
                        if ok {
                            self.logged_in = true;
                            self.username = if self.page == Page::Register {
                                self.reg_username.clone()
                            } else {
                                self.username_input.clone()
                            };
                            self.status = format!("Logged in as {}", self.username);
                            self.auth_feedback.clear();
                            // Networking task will query user list via auth protocol
                        } else {
                            self.auth_feedback = message;
                        }
                        ctx.request_repaint();
                    }
                    NetToUi::Users(map) => {
                        // Remove our own username from the directory so we can't select ourselves
                        let mut map = map;
                        if !self.username.is_empty() {
                            map.remove(&self.username);
                        }
                        // Rebuild forward and reverse maps
                        self.peer_to_username.clear();
                        for (uname, pid) in &map { self.peer_to_username.insert(pid.clone(), uname.clone()); }
                        self.conversations.retain(|user, _| map.contains_key(user));
                        self.users = map;
                        for name in self.users.keys() {
                            self.conversations.entry(name.clone()).or_default();
                        }
                        // reset selected if missing
                        if let Some(name) = self.selected_user.clone() {
                            if !self.users.contains_key(&name) { self.selected_user = None; }
                        }
                        ctx.request_repaint();
                    }
                    NetToUi::DeleteResult { ok, message } => {
                        if ok {
                            // Reset to login
                            self.logged_in = false;
                            self.username.clear();
                            self.selected_user = None;
                            self.users.clear();
                            self.peer_to_username.clear();
                            self.message_input.clear();
                            self.conversations.clear();
                            self.show_delete_view = false;
                            self.page = Page::Login;
                            self.auth_feedback = "Account deleted".to_string();
                        } else {
                            self.del_feedback = message;
                        }
                        ctx.request_repaint();
                    }
                }
            }

            // Login/Register gate UI
            if !self.logged_in {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(32.0);
                        match self.page {
                            Page::Login => {
                                ui.heading("Login");
                                ui.add_space(8.0);
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.username_input)
                                        .hint_text("Username")
                                        .desired_width(360.0)
                                );
                                ui.add_space(6.0);
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.password_input)
                                        .hint_text("Password")
                                        .password(true)
                                        .desired_width(360.0)
                                );
                                ui.add_space(10.0);
                                // Manually center the buttons within a fixed-width container
                                ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                                    ui.set_width(360.0); // Match the width of the text inputs
                                    ui.horizontal(|ui| {
                                        // Calculate padding to center the buttons
                                        let button_width = BUTTON_WIDTH * 2.0 + ui.spacing().item_spacing.x;
                                        let padding = (ui.available_width() - button_width) / 2.0;
                                        ui.add_space(padding);

                                        let login = ui.add_sized([BUTTON_WIDTH, UI_HEIGHT], egui::Button::new("Login")).clicked();
                                        let register = ui.add_sized([BUTTON_WIDTH, UI_HEIGHT], egui::Button::new("Register")).clicked();

                                        if login {
                                            if self.username_input.trim().is_empty() || self.password_input.is_empty() {
                                                self.auth_feedback = "Username and password required".to_string();
                                            } else {
                                                let _ = self.tx.send(UiToNet::Login { username: self.username_input.trim().to_string(), password: self.password_input.clone() });
                                                self.auth_feedback = "Logging in...".to_string();
                                            }
                                        }
                                        if register {
                                            self.page = Page::Register;
                                            self.reg_username = self.username_input.clone();
                                        }
                                    });
                                });
                                ui.add_space(6.0);
                                if !self.auth_feedback.is_empty() { ui.colored_label(egui::Color32::YELLOW, &self.auth_feedback); }
                            }
                            Page::Register => {
                                ui.heading("Register");
                                ui.add_space(8.0);
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.reg_username)
                                        .hint_text("Username")
                                        .desired_width(360.0)
                                );
                                ui.add_space(6.0);
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.reg_password)
                                        .hint_text("Password")
                                        .password(true)
                                        .desired_width(360.0)
                                );
                                // Pull birthdate row closer to password field
                                ui.add_space(2.0);
                                // Center the birthdate chooser inside a 360px container (symmetric around vertical axis)
                                ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                                    ui.set_width(360.0);
                                    // Use consistent widths per dropdown so the row is symmetric
                                    ui.horizontal(|ui| {
                                        let combo_w: f32 = 110.0;
                                        let total = combo_w * 3.0 + 2.0 * ui.spacing().item_spacing.x;
                                        let left_pad = (ui.available_width() - total).max(0.0) / 2.0;
                                        ui.add_space(left_pad);
                                        // Year selector (1900..=2025)
                                        egui::ComboBox::from_id_source("year_combo").width(combo_w)
                                            .selected_text(format!("Year: {}", self.reg_birth_year))
                                            .show_ui(ui, |ui| {
                                                for y in (1900..=2025).rev() {
                                                    if ui.selectable_label(self.reg_birth_year == y, y.to_string()).clicked() {
                                                        self.reg_birth_year = y;
                                                        // Clamp day when year changes (for Feb/leap year)
                                                        let max_day = days_in_month(self.reg_birth_year, self.reg_birth_month);
                                                        if self.reg_birth_day > max_day { self.reg_birth_day = max_day; }
                                                    }
                                                }
                                            });

                                        // Month selector (1..=12)
                                        const MONTH_NAMES: [&str; 12] = [
                                            "Jan", "Feb", "Mar", "Apr", "May", "Jun",
                                            "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
                                        ];
                                        // Compute safe index for month name (1..=12)
                                        let month_idx = (self.reg_birth_month.clamp(1, 12) - 1) as usize;
                                        egui::ComboBox::from_id_source("month_combo").width(combo_w)
                                            .selected_text(format!("Month: {}", MONTH_NAMES[month_idx]))
                                            .show_ui(ui, |ui| {
                                                for m in 1..=12u32 {
                                                    let label = MONTH_NAMES[m as usize - 1];
                                                    if ui.selectable_label(self.reg_birth_month == m, label).clicked() {
                                                        self.reg_birth_month = m;
                                                        // Clamp day when month changes
                                                        let max_day = days_in_month(self.reg_birth_year, self.reg_birth_month);
                                                        if self.reg_birth_day > max_day { self.reg_birth_day = max_day; }
                                                    }
                                                }
                                            });

                                        // Day selector based on month/year
                                        let max_day = days_in_month(self.reg_birth_year, self.reg_birth_month);
                                        egui::ComboBox::from_id_source("day_combo").width(combo_w)
                                            .selected_text(format!("Day: {}", self.reg_birth_day))
                                            .show_ui(ui, |ui| {
                                                for d in 1..=max_day {
                                                    if ui.selectable_label(self.reg_birth_day == d, d.to_string()).clicked() {
                                                        self.reg_birth_day = d;
                                                    }
                                                }
                                            });
                                    });
                                });
                                // Small gap before the action buttons
                                ui.add_space(4.0);
                                // Center action buttons inside the same 360px container, like login page
                                ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                                    ui.set_width(360.0);
                                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                        let total = 2.0 * BUTTON_WIDTH + ui.spacing().item_spacing.x;
                                        let left_pad = (ui.available_width() - total).max(0.0) / 2.0;
                                        ui.add_space(left_pad);
                                        let submit = ui.add_sized([BUTTON_WIDTH, UI_HEIGHT], egui::Button::new("Create Account")).clicked();
                                        let back = ui.add_sized([BUTTON_WIDTH, UI_HEIGHT], egui::Button::new("Back to Login")).clicked();
                                    if submit {
                                        // Format birthdate as YYYY-MM-DD
                                        let birthdate = format!(
                                            "{:04}-{:02}-{:02}",
                                            self.reg_birth_year,
                                            self.reg_birth_month,
                                            self.reg_birth_day
                                        );
                                        if self.reg_username.trim().is_empty() || self.reg_password.is_empty() {
                                            self.auth_feedback = "Fill all fields".to_string();
                                        } else {
                                            let _ = self.tx.send(UiToNet::Register {
                                                username: self.reg_username.trim().to_string(),
                                                password: self.reg_password.clone(),
                                                birthdate,
                                            });
                                            self.auth_feedback = "Registering...".to_string();
                                        }
                                        }
                                        if back { self.page = Page::Login; }
                                    });
                                });
                                ui.add_space(6.0);
                                if !self.auth_feedback.is_empty() { ui.colored_label(egui::Color32::YELLOW, &self.auth_feedback); }
                            }
                        }
                    });
                });
                return;
            }

            // Account deletion modal takes over the layout when toggled
            if self.show_delete_view {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(24.0);
                        ui.heading("Delete Account");
                        ui.label("Enter your credentials to permanently delete your account.");
                        ui.add_space(12.0);
                        ui.add(
                            egui::TextEdit::singleline(&mut self.del_username)
                                .hint_text("Username")
                                .desired_width(360.0),
                        );
                        ui.add_space(6.0);
                        ui.add(
                            egui::TextEdit::singleline(&mut self.del_password)
                                .hint_text("Password")
                                .password(true)
                                .desired_width(360.0),
                        );
                        ui.add_space(12.0);
                        ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                            ui.set_width(360.0);
                            ui.horizontal(|ui| {
                                let total = 2.0 * BUTTON_WIDTH + ui.spacing().item_spacing.x;
                                let left_pad = (ui.available_width() - total).max(0.0) / 2.0;
                                ui.add_space(left_pad);
                                let confirm = ui
                                    .add_sized([BUTTON_WIDTH, UI_HEIGHT], egui::Button::new("Delete"))
                                    .clicked();
                                let cancel = ui
                                    .add_sized([BUTTON_WIDTH, UI_HEIGHT], egui::Button::new("Cancel"))
                                    .clicked();
                                if confirm {
                                    if self.del_username.trim().is_empty() || self.del_password.is_empty() {
                                        self.del_feedback = "Username and password required".to_string();
                                    } else {
                                        let _ = self.tx.send(UiToNet::DeleteAccount {
                                            username: self.del_username.trim().to_string(),
                                            password: self.del_password.clone(),
                                        });
                                        self.del_feedback = "Deleting account...".to_string();
                                    }
                                }
                                if cancel {
                                    self.show_delete_view = false;
                                    self.del_feedback.clear();
                                }
                            });
                        });
                        ui.add_space(8.0);
                        if !self.del_feedback.is_empty() {
                            ui.colored_label(egui::Color32::YELLOW, &self.del_feedback);
                        }
                    });
                });
                return;
            }

            let mut logout_requested = false;

            egui::TopBottomPanel::top("chat_top_bar").show(ctx, |ui| {
                egui::Frame::none()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::same(12.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(egui::RichText::new(&self.username).heading());
                                ui.label(egui::RichText::new(&self.status).small());
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui
                                    .add_sized([BUTTON_WIDTH, UI_HEIGHT], egui::Button::new("Logout"))
                                    .clicked()
                                {
                                    logout_requested = true;
                                }

                                if ui
                                    .add_sized([BUTTON_WIDTH, UI_HEIGHT], egui::Button::new("Account"))
                                    .clicked()
                                {
                                    self.show_delete_view = true;
                                    self.del_username = self.username.clone();
                                    self.del_password.clear();
                                    self.del_feedback.clear();
                                }
                            });
                        });
                    });
            });

            if logout_requested {
                if !self.username.is_empty() {
                    let _ = self.tx.send(UiToNet::Logout {
                        username: self.username.clone(),
                    });
                }
                self.logged_in = false;
                self.username.clear();
                self.username_input.clear();
                self.password_input.clear();
                self.selected_user = None;
                self.users.clear();
                self.peer_to_username.clear();
                self.message_input.clear();
                self.conversations.clear();
                self.status = "Logged out".to_string();
                self.page = Page::Login;
                self.auth_feedback.clear();
                self.show_delete_view = false;
                return;
            }

            egui::SidePanel::left("chat_sidebar")
                .resizable(false)
                .min_width(260.0)
                .show(ctx, |ui| {
                    ui.heading("Chats");
                    ui.add_space(8.0);

                    if self.users.is_empty() {
                        ui.label("No peers available yet. Stay tuned while discovery runs...");
                    }

                    let mut names: Vec<String> = self.users.keys().cloned().collect();
                    names.sort_by(|a, b| {
                        let convo_a = self.conversations.get(a);
                        let convo_b = self.conversations.get(b);

                        let unread_a = convo_a.map(|c| c.unread).unwrap_or(false);
                        let unread_b = convo_b.map(|c| c.unread).unwrap_or(false);
                        let time_a = convo_a.map(|c| c.last_activity).unwrap_or(SystemTime::UNIX_EPOCH);
                        let time_b = convo_b.map(|c| c.last_activity).unwrap_or(SystemTime::UNIX_EPOCH);

                        unread_b
                            .cmp(&unread_a)
                            .then_with(|| time_b.cmp(&time_a))
                            .then_with(|| a.to_lowercase().cmp(&b.to_lowercase()))
                    });

                    for name in names {
                        let conversation = self.conversations.get(&name);
                        let preview = conversation
                            .and_then(|conv| conv.messages.last())
                            .map(|msg| {
                                let prefix = if msg.from_self { "You" } else { name.as_str() };
                                format!("{}: {}", prefix, truncate_preview(&msg.text))
                            })
                            .unwrap_or_else(|| "No messages yet".to_string());

                        let is_selected = self
                            .selected_user
                            .as_ref()
                            .map(|selected| selected == &name)
                            .unwrap_or(false);
                        let is_unread = conversation.map(|c| c.unread).unwrap_or(false);

                        let desired_size = egui::vec2(ui.available_width(), 70.0);
                        let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::click());
                        let mut visuals = ui.style().interact_selectable(&response, is_selected);
                        if is_unread && !is_selected {
                            visuals.bg_fill = egui::Color32::from_rgb(56, 142, 60);
                            visuals.bg_stroke = egui::Stroke { width: 1.0, color: egui::Color32::from_rgb(67, 160, 71) };
                        }
                        ui.painter().rect(
                            rect,
                            egui::Rounding::same(RADIUS),
                            visuals.bg_fill,
                            visuals.bg_stroke,
                        );

                        let inner = rect.shrink2(egui::vec2(12.0, 10.0));
                        let mut child_ui = ui.child_ui(inner, egui::Layout::top_down(egui::Align::LEFT));
                        child_ui.label(egui::RichText::new(&name).strong());
                        child_ui.label(egui::RichText::new(preview).small());

                        if response.clicked() {
                            let conv = self.conversations.entry(name.clone()).or_default();
                            conv.unread = false;
                            if self.selected_user.as_ref() != Some(&name) {
                                self.selected_user = Some(name.clone());
                                self.status = format!("Connecting to {}...", name);
                                if let Some(pid) = self.users.get(&name).cloned() {
                                    let _ = self.tx.send(UiToNet::Connect { peer_id: pid });
                                }
                            }
                            ui.ctx().request_repaint();
                        }
                        ui.add_space(6.0);
                    }
                });

            let selected_user = self.selected_user.clone();

            egui::TopBottomPanel::bottom("chat_input_panel").show(ctx, |ui| {
                egui::Frame::none()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::same(10.0))
                    .show(ui, |ui| {
                        ui.separator();
                        let can_chat = selected_user.is_some();
                        ui.add_space(4.0);
                        ui.add_enabled_ui(can_chat, |ui| {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let send_clicked = ui
                                    .add_sized(
                                        [BUTTON_WIDTH, UI_HEIGHT],
                                        egui::Button::new(egui::RichText::new("Send").color(egui::Color32::WHITE))
                                            .fill(egui::Color32::from_rgb(255, 152, 0))
                                            .rounding(egui::Rounding::same(RADIUS))
                                            .stroke(egui::Stroke { width: 1.0, color: egui::Color32::from_rgb(230, 130, 0) }),
                                    )
                                    .clicked();

                                let input_id = egui::Id::new("chat_input_field");
                                let text_edit = egui::TextEdit::multiline(&mut self.message_input)
                                    .id_source(input_id)
                                    .desired_rows(5)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("Type a message...")
                                    .frame(false);

                                let inner = egui::Frame::none()
                                    .fill(egui::Color32::from_rgb(38, 43, 50))
                                    .rounding(egui::Rounding::same(RADIUS))
                                    .stroke(egui::Stroke { width: 1.0, color: egui::Color32::from_rgb(55, 61, 69) })
                                    .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                                    .show(ui, |ui| {
                                        let w = ui.available_width();
                                        let row_h = ui.text_style_height(&egui::TextStyle::Body);
                                        let fixed_h = row_h * 5.0;
                                        ui.set_min_height(fixed_h);
                                        ui.set_max_height(fixed_h);

                                        egui::ScrollArea::vertical()
                                            .auto_shrink([false, false])
                                            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                                            .max_height(fixed_h)
                                            .show(ui, |ui| {
                                                ui.set_width(w);
                                                ui.add(text_edit);
                                            });
                                    });
                                let _ = inner.inner;

                                if send_clicked {
                                    if let Some(name) = selected_user.clone() {
                                        if let Some(peer_id) = self.users.get(&name).cloned() {
                                            let message = self.message_input.trim();
                                            if !message.is_empty() {
                                                let message = message.to_string();
                                                let _ = self.tx.send(UiToNet::Write {
                                                    peer_id,
                                                    from_username: self.username.clone(),
                                                    to_username: name.clone(),
                                                    msg: message,
                                                });
                                                self.message_input.clear();
                                            }
                                        }
                                    }
                                }
                            });
                        });
                        if !can_chat {
                            ui.label("Select a conversation to start chatting.");
                        }
                    });
            });

            egui::CentralPanel::default().show(ctx, |ui| {
                ui.set_width(ui.available_width());
                ui.add_space(8.0);
                if let Some(name) = selected_user {
                    ui.heading(&name);
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical()
                        .id_source("chat_scroll")
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                        .auto_shrink([false, false])
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            if let Some(conversation) = self.conversations.get(&name) {
                                for msg in &conversation.messages {
                                    let row_width = ui.available_width();
                                    let layout = if msg.from_self {
                                        egui::Layout::right_to_left(egui::Align::Min)
                                    } else {
                                        egui::Layout::left_to_right(egui::Align::Min)
                                    };
                                    ui.allocate_ui_with_layout(egui::vec2(row_width, 0.0), layout, |ui| {
                                        let (fill, stroke) = if msg.from_self {
                                            (
                                                egui::Color32::from_rgb(25, 118, 210),
                                                egui::Color32::from_rgb(21, 101, 192),
                                            )
                                        } else {
                                            (
                                                egui::Color32::from_rgb(38, 43, 50),
                                                egui::Color32::from_rgb(55, 61, 69),
                                            )
                                        };
                                        egui::Frame::none()
                                            .fill(fill)
                                            .rounding(egui::Rounding::same(RADIUS))
                                            .stroke(egui::Stroke { width: 1.0, color: stroke })
                                            .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                                            .show(ui, |ui| {
                                                let author = if msg.from_self { "You" } else { name.as_str() };
                                                ui.colored_label(egui::Color32::WHITE, egui::RichText::new(author).small());
                                                ui.add_space(2.0);
                                                ui.colored_label(egui::Color32::WHITE, &msg.text);
                                            });
                                    });
                                    ui.add_space(6.0);
                                }
                            } else {
                                ui.vertical_centered(|ui| {
                                    ui.add_space(40.0);
                                    ui.label("No messages yet. Say hi!");
                                });
                            }
                        });
                } else {
                    ui.vertical_centered(|ui| {
                        ui.add_space(80.0);
                        ui.heading("No chat selected");
                        ui.label("Pick a user from the left to begin chatting.");
                    });
                }
            });
        }

    }

    impl Drop for ChatApp {
        fn drop(&mut self) {
            // Best-effort: attempt to inform server we're logging out.
            if self.logged_in && !self.username.is_empty() {
                let _ = self.tx.send(UiToNet::Logout { username: self.username.clone() });
            }
        }
    }

    // --- Networking task ---
    async fn network_task(mut rx: UnboundedReceiver<UiToNet>, tx: UnboundedSender<NetToUi>, rendezvous_point_address: Multiaddr) {
        let _ = tx.send(NetToUi::Info("Starting networking...".into()));

        let local_key = libp2p::identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(local_key.public());
    // Intentionally do not send local peer id to UI

        let mut swarm = match libp2p::SwarmBuilder::with_existing_identity(local_key)
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            ) {
            Ok(builder) => {
                let builder = match builder.with_behaviour(|key| {
                    let rr_cfg = request_response::Config::default()
                        .with_request_timeout(std::time::Duration::from_secs(30))
                        .with_max_concurrent_streams(usize::MAX);
                    let auth_cfg = request_response::Config::default()
                        .with_request_timeout(std::time::Duration::from_secs(15))
                        .with_max_concurrent_streams(16);
                    ClientBehaviour {
                        rendezvous: rendezvous::client::Behaviour::new(key.clone()),
                        ping: ping::Behaviour::new(ping::Config::default()),
                        identify: identify::Behaviour::new(identify::Config::new(
                            "/p2p-client/1.0.0".to_string(),
                            key.public(),
                        )),
                        request_response: request_response::Behaviour::new(
                            std::iter::once((HelloProtocol(), request_response::ProtocolSupport::Full)),
                            rr_cfg,
                        ),
                        auth: request_response::Behaviour::new(
                            std::iter::once((AuthProtocol(), request_response::ProtocolSupport::Full)),
                            auth_cfg,
                        ),
                    }
                }) {
                    Ok(b) => b,
                    Err(e) => { let _ = tx.send(NetToUi::Error(format!("Behaviour: {}", e))); return; }
                };
                builder
                    .with_swarm_config(|c: libp2p::swarm::Config| c.with_idle_connection_timeout(std::time::Duration::from_secs(60)))
                    .build()
            }
            Err(e) => { let _ = tx.send(NetToUi::Error(format!("Transport: {}", e))); return; }
        };

        if let Err(e) = swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse().unwrap()) {
            let _ = tx.send(NetToUi::Error(format!("listen_on error: {}", e)));
        }

    let rendezvous_point_peer_id = PeerId::from_str("12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN").unwrap();

        if let Err(e) = swarm.dial(rendezvous_point_address.clone()) {
            let _ = tx.send(NetToUi::Error(format!("Dial rendezvous failed: {}", e)));
        }

    let mut discovered: HashMap<PeerId, Vec<Multiaddr>> = HashMap::new();
    let mut connected: HashSet<PeerId> = HashSet::new();
    let mut is_registered = false;
    let mut is_authenticated = false;
    // Reverse map of PeerId -> username for display of incoming messages
    let mut peer_to_username_net: HashMap<String, String> = HashMap::new();

        // Periodic rediscovery every 5s for a more responsive UI
    let mut rediscover_interval = tokio::time::interval(std::time::Duration::from_secs(5));
    let mut users_refresh_interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            tokio::select! {
                Some(cmd) = rx.recv() => {
                    match cmd {
                        UiToNet::Connect { peer_id } => {
                            if let Ok(peer) = PeerId::from_str(&peer_id) {
                                if peer == rendezvous_point_peer_id { let _=tx.send(NetToUi::Info("Cannot connect to rendezvous server".into())); continue; }
                                if let Some(addrs) = discovered.get(&peer) {
                                    for addr in addrs {
                                        // Feed address to swarm peer address book and dial
                                        swarm.add_peer_address(peer, addr.clone());
                                        let _=swarm.dial(addr.clone());
                                    }
                                } else { let _=tx.send(NetToUi::Info("Peer not discovered yet".into())); }
                            } else { let _=tx.send(NetToUi::Error("Invalid PeerId".into())); }
                        }
                        UiToNet::Write { peer_id, from_username, to_username, msg } => {
                            if let Ok(peer) = PeerId::from_str(&peer_id) {
                                if !connected.contains(&peer) {
                                    if let Some(addrs) = discovered.get(&peer) { for addr in addrs { let _=swarm.dial(addr.clone()); } }
                                }
                                // Echo to local chat window immediately
                                let _ = tx.send(NetToUi::ChatMessage {
                                    peer: to_username.clone(),
                                    direction: MessageDirection::Outgoing,
                                    text: msg.clone(),
                                });
                                // Wrap the message with the sender's username so the receiver can always display name
                                let payload = format!("MSG:{}|{}", from_username, msg);
                                swarm.behaviour_mut().request_response.send_request(&peer, payload);
                            } else { let _=tx.send(NetToUi::Error("Invalid PeerId".into())); }
                        }
                        UiToNet::Register { username, password, birthdate } => {
                            let payload = format!("REGISTER:{}|{}|{}", username, password, birthdate);
                            swarm.behaviour_mut().auth.send_request(&rendezvous_point_peer_id, payload);
                        }
                        UiToNet::Login { username, password } => {
                            let payload = format!("LOGIN:{}|{}", username, password);
                            swarm.behaviour_mut().auth.send_request(&rendezvous_point_peer_id, payload);
                        }
                        UiToNet::Logout { username } => {
                            let payload = format!("LOGOUT:{}", username);
                            let _ = swarm.behaviour_mut().auth.send_request(&rendezvous_point_peer_id, payload);
                        }
                        UiToNet::DeleteAccount { username, password } => {
                            let payload = format!("DELETE:{}|{}", username, password);
                            let _ = swarm.behaviour_mut().auth.send_request(&rendezvous_point_peer_id, payload);
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
                            let _ = tx.send(NetToUi::Connected(peer_id.to_string()));
                        }
                        SwarmEvent::ConnectionClosed { peer_id, .. } => {
                            tracing::info!("Disconnected from {}", peer_id);
                            connected.remove(&peer_id);
                            let _ = tx.send(NetToUi::Disconnected(peer_id.to_string()));
                            // If this was the rendezvous server, clear our user list (will repopulate if we reconnect)
                            if peer_id == rendezvous_point_peer_id {
                                let _ = tx.send(NetToUi::Users(HashMap::new()));
                                peer_to_username_net.clear();
                            }
                        }
                        SwarmEvent::Behaviour(ClientBehaviourEvent::Identify(identify::Event::Received { peer_id, info, })) => {
                            tracing::info!("Received identify info from {}: observed address {:?}", peer_id, info.observed_addr);
                            if peer_id == rendezvous_point_peer_id && !is_registered {
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
                            is_registered = true;
                            let _ = swarm.behaviour_mut().rendezvous.discover(
                                Some(rendezvous::Namespace::new(RENDEZVOUS_NAMESPACE.to_string()).unwrap()),
                                None,
                                None,
                                rendezvous_point_peer_id
                            );
                        }
                        SwarmEvent::Behaviour(ClientBehaviourEvent::Rendezvous(rendezvous::client::Event::Discovered { registrations, .. })) => {
                            for registration in registrations {
                                let discovered_peer = registration.record.peer_id();
                                if discovered_peer == local_peer_id { continue; }
                                let entry = discovered.entry(discovered_peer).or_default();
                                for address in registration.record.addresses() {
                                    if !entry.contains(address) {
                                        entry.push(address.clone());
                                        swarm.add_peer_address(discovered_peer, address.clone());
                                    }
                                }
                            }
                            let list: Vec<String> = discovered.keys().map(|p| p.to_string()).collect();
                            let _ = tx.send(NetToUi::Discovered(list));
                        }
                        // Chat RequestResponse
                        SwarmEvent::Behaviour(ClientBehaviourEvent::RequestResponse(event)) => match event {
                            request_response::Event::Message { peer, message } => {
                                match message {
                                    request_response::Message::Request { request, channel, .. } => {
                                        let request_str = request.to_string();
                                        // Try to parse embedded username: format "MSG:<from_username>|<text>"
                                        if let Some(rest) = request_str.strip_prefix("MSG:") {
                                            if let Some((from_name, text)) = rest.split_once('|') {
                                                // Update reverse map for future lookups and display
                                                let peer_key = peer.to_string();
                                                peer_to_username_net.insert(peer_key, from_name.to_string());
                                                let _ = tx.send(NetToUi::ChatMessage {
                                                    peer: from_name.to_string(),
                                                    direction: MessageDirection::Incoming,
                                                    text: text.to_string(),
                                                });
                                            } else {
                                                // Malformed payload, fallback to known mapping without exposing PeerId
                                                let peer_key = peer.to_string();
                                                let from_label = peer_to_username_net.get(&peer_key).cloned().unwrap_or_else(|| "Unknown".to_string());
                                                let _ = tx.send(NetToUi::ChatMessage {
                                                    peer: from_label.clone(),
                                                    direction: MessageDirection::Incoming,
                                                    text: request_str.clone(),
                                                });
                                            }
                                        } else {
                                            // Backward compatibility: old clients may send plain text. Use mapping if available, otherwise show "Unknown".
                                            let peer_key = peer.to_string();
                                            let from_label = peer_to_username_net.get(&peer_key).cloned().unwrap_or_else(|| "Unknown".to_string());
                                            let _ = tx.send(NetToUi::ChatMessage {
                                                peer: from_label,
                                                direction: MessageDirection::Incoming,
                                                text: request_str.clone(),
                                            });
                                        }
                                        // Respond with a small ack so the sender gets a response per message
                                        if let Err(e) = swarm.behaviour_mut().request_response.send_response(channel, "ok".to_string()) {
                                            tracing::error!("Failed to send response: {}", e);
                                        }
                                    }
                                    request_response::Message::Response { response, .. } => {
                                        // Surface responses without exposing peer id
                                        let _ = tx.send(NetToUi::Info(format!("Response received: {}", response)));
                                    }
                                }
                            }
                            request_response::Event::OutboundFailure { peer, error, request_id: _ } => {
                                tracing::error!("Outbound request to {} failed: {:?}", peer, error);
                                let _ = tx.send(NetToUi::Error(format!("Outbound request failed: {:?}", error)));
                            }
                            request_response::Event::InboundFailure { peer, error, request_id: _ } => {
                                tracing::error!("Inbound with {} failed: {:?}", peer, error);
                                let _ = tx.send(NetToUi::Error(format!("Inbound request failed: {:?}", error)));
                            }
                            request_response::Event::ResponseSent { peer, .. } => {
                                tracing::debug!("Response sent to {}", peer);
                            }
                        },
                        // Auth RequestResponse
                        SwarmEvent::Behaviour(ClientBehaviourEvent::Auth(event)) => match event {
                            request_response::Event::Message { peer: _, message } => {
                                if let request_response::Message::Response { response, .. } = message {
                                    if let Some(rest) = response.strip_prefix("AUTH:") {
                                        let ok = rest.starts_with("OK");
                                        let msg = if ok { "Authenticated".to_string() } else { rest.strip_prefix("ERR:").unwrap_or(rest).to_string() };
                                        let _ = tx.send(NetToUi::AuthResult { ok, message: msg });
                                        if ok {
                                            is_authenticated = true;
                                            // After successful auth, request the user list via auth protocol
                                            let _ = swarm.behaviour_mut().auth.send_request(&rendezvous_point_peer_id, "LIST".to_string());
                                        }
                                    } else if let Some(rest) = response.strip_prefix("LIST:") {
                                        // Parse username=peerid pairs separated by commas
                                        let mut map = HashMap::new();
                                        peer_to_username_net.clear();
                                        if !rest.is_empty() {
                                            for pair in rest.split(',') {
                                                if let Some((name, pid)) = pair.split_once('=') {
                                                    let uname = name.to_string();
                                                    let pid_str = pid.to_string();
                                                    map.insert(uname.clone(), pid_str.clone());
                                                    peer_to_username_net.insert(pid_str, uname);
                                                }
                                            }
                                        }
                                        let _ = tx.send(NetToUi::Users(map));
                                    } else if let Some(rest) = response.strip_prefix("DELETE:") {
                                        // DELETE:OK or DELETE:ERR:reason
                                        let ok = rest.starts_with("OK");
                                        let msg = if ok { "Account deleted".to_string() } else { rest.strip_prefix("ERR:").unwrap_or(rest).to_string() };
                                        let _ = tx.send(NetToUi::DeleteResult { ok, message: msg });
                                    } else {
                                        // Backward-compat: older server without AUTH: prefix
                                        let ok = response.starts_with("OK");
                                        let msg = if ok { "Authenticated".to_string() } else { response.trim_start_matches("ERR:").to_string() };
                                        let _ = tx.send(NetToUi::AuthResult { ok, message: msg });
                                    }
                                }
                            }
                            request_response::Event::OutboundFailure { peer: _, error, .. } => {
                                let _ = tx.send(NetToUi::AuthResult { ok: false, message: format!("Auth request failed: {:?}", error) });
                            }
                            _ => {}
                        },
                        _ => {}
                    }
                }
                // Periodic rediscovery tick
                _ = rediscover_interval.tick() => {
                    if is_registered {
                        let _ = swarm.behaviour_mut().rendezvous.discover(
                            Some(rendezvous::Namespace::new(RENDEZVOUS_NAMESPACE.to_string()).unwrap()),
                            None,
                            None,
                            rendezvous_point_peer_id
                        );
                    }
                }
                // Periodic user list refresh after authentication
                _ = users_refresh_interval.tick() => {
                    if is_authenticated {
                        let _ = swarm.behaviour_mut().auth.send_request(&rendezvous_point_peer_id, "LIST".to_string());
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
        auth: request_response::Behaviour<AuthCodec>,
    }

    fn truncate_preview(text: &str) -> String {
        const MAX_LEN: usize = 48;
        let mut cleaned = String::with_capacity(text.len());
        for ch in text.chars() {
            if ch == '\n' {
                cleaned.push(' ');
            } else {
                cleaned.push(ch);
            }
            if cleaned.len() >= MAX_LEN {
                cleaned.truncate(MAX_LEN);
                cleaned.push('…');
                return cleaned;
            }
        }
        cleaned
    }

    // --- Utilities for Register date picker ---
    fn is_leap_year(year: i32) -> bool {
        (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
    }

    fn days_in_month(year: i32, month: u32) -> u32 {
        match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => if is_leap_year(year) { 29 } else { 28 },
            _ => 30,
        }
    }