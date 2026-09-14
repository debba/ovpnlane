use crate::{
    netstack::{PacketSink, Tunnel},
    settings::Settings,
};
use openvpn_connect::{
    Event, EventHandler, ExternalTun, ExternalTunConfig, ExternalTunInfo, ExternalTunIo,
    ExternalTunStartConfig, Status,
};
use std::{
    io,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
};

struct CoreSink(ExternalTunIo);
impl PacketSink for CoreSink {
    fn send(&self, packet: &[u8]) -> io::Result<()> {
        self.0.receive(packet).map_err(io::Error::other)
    }
}

#[derive(Default)]
struct State {
    generation: u64,
    mtu: usize,
    io: Option<ExternalTunIo>,
    info: ExternalTunInfo,
}

#[derive(Default)]
struct DiagnosticState {
    received_reply: bool,
    certificate_failed: bool,
    failure: Option<String>,
}

/// Core can return an empty success status even after a fatal event. Retain the
/// reason independently of the subsequent DISCONNECTED callback.
#[derive(Clone, Default)]
pub struct Diagnostics(Arc<Mutex<DiagnosticState>>);

impl Diagnostics {
    fn record(&self, event: &Event) {
        let mut state = self.0.lock().unwrap();
        match event.name.as_str() {
            "CONNECTING" => state.received_reply = true,
            "CONNECTED" => {
                state.received_reply = true;
                state.certificate_failed = false;
                state.failure = None;
            }
            _ => {}
        }
        if event.error || event.fatal {
            state.failure = Some(match event.name.as_str() {
                "AUTH_FAILED" => "AUTH_FAILED: the VPN server rejected authentication; check VPN credentials, MFA requirements and server policy".into(),
                "CONNECTION_TIMEOUT" if state.certificate_failed => "CONNECTION_TIMEOUT: VPN server certificate verification failed".into(),
                "CONNECTION_TIMEOUT" if !state.received_reply => "CONNECTION_TIMEOUT: no reply received from the VPN server; check the remote address, port/protocol, network reachability and tls-auth/tls-crypt settings".into(),
                "CONNECTION_TIMEOUT" => "CONNECTION_TIMEOUT: the VPN server replied, but connection setup did not complete before the timeout".into(),
                _ => format!("{}: VPN connection failed", event_code(&event.name)),
            });
        }
    }

    pub fn session_end(&self, status: &Status) -> String {
        let state = self.0.lock().unwrap();
        if let Some(failure) = &state.failure {
            return format!("VPN session ended: {failure}");
        }
        if state.certificate_failed {
            return "VPN session ended: VPN server certificate verification failed".into();
        }
        // Do not copy arbitrary native status messages: they may carry tokens.
        if !status.status.is_empty() {
            return format!("VPN session ended: {}", event_code(&status.status));
        }
        "VPN session ended without a reported error; run with --verbose to see connection events"
            .into()
    }
}

fn event_code(name: &str) -> &str {
    if !name.is_empty()
        && name.len() <= 80
        && name
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
    {
        name
    } else {
        "OPENVPN_ERROR"
    }
}

fn transport_contact(text: &str) -> Option<(SocketAddr, &str)> {
    let (endpoint, protocol) = text
        .trim()
        .strip_prefix("Contacting ")?
        .split_once(" via ")?;
    if !matches!(
        protocol,
        "UDP" | "UDPv4" | "UDPv6" | "TCP" | "TCPv4" | "TCPv6"
    ) {
        return None;
    }
    Some((endpoint.parse().ok()?, protocol))
}

pub struct UserspaceVpn {
    tunnel: Tunnel,
    dns: Vec<IpAddr>,
    state: Mutex<State>,
    diagnostics: Diagnostics,
}
impl UserspaceVpn {
    pub fn new(tunnel: Tunnel, dns: Vec<IpAddr>) -> Self {
        Self {
            tunnel,
            dns,
            state: Mutex::new(State::default()),
            diagnostics: Diagnostics::default(),
        }
    }

    pub fn diagnostics(&self) -> Diagnostics {
        self.diagnostics.clone()
    }
}
impl EventHandler for UserspaceVpn {
    fn log(&self, text: &str) {
        if let Some((endpoint, protocol)) = transport_contact(text) {
            tracing::debug!(%endpoint, %protocol, "Contacting VPN server");
        }
        // Surface a useful TLS diagnostic without copying arbitrary Core log payloads.
        let lower = text.to_ascii_lowercase();
        if lower.contains("certificate verification failed")
            || lower.contains("certificate verify failed")
            || lower.contains("verify-x509-name did not match")
        {
            self.diagnostics.0.lock().unwrap().certificate_failed = true;
            tracing::error!("VPN server certificate verification failed");
        }
    }

    fn event(&self, event: Event) {
        self.diagnostics.record(&event);
        // Event payloads and Core logs can contain authentication tokens; omit them.
        if event.error || event.fatal {
            tracing::error!(event = %event.name, "OpenVPN event");
        } else {
            tracing::info!(event = %event.name, "OpenVPN event");
        }
    }
    fn external_tun(&self) -> Option<&dyn ExternalTun> {
        Some(self)
    }
}

impl ExternalTun for UserspaceVpn {
    fn configure(&self, config: &ExternalTunConfig) -> bool {
        if config.layer != 3 {
            return false;
        }
        self.state.lock().unwrap().mtu = if config.mtu > 0 {
            config.mtu as usize
        } else {
            1500
        };
        true
    }
    fn start(&self, config: &ExternalTunStartConfig, io: ExternalTunIo) {
        let settings =
            match Settings::parse(&config.options, self.state.lock().unwrap().mtu, &self.dns) {
                Ok(settings) => settings,
                Err(error) => {
                    let _ = io.error(&error.to_string());
                    return;
                }
            };
        let generation = {
            let mut state = self.state.lock().unwrap();
            state.generation += 1;
            state.info = ExternalTunInfo {
                name: "userspace".into(),
                vpn_ipv4: settings.ipv4.map(|ip| ip.to_string()).unwrap_or_default(),
                vpn_ipv6: settings.ipv6.map(|ip| ip.to_string()).unwrap_or_default(),
                mtu: settings.mtu as i32,
                ..ExternalTunInfo::default()
            };
            state.io = Some(io.clone());
            state.generation
        };
        tracing::info!(ipv4 = ?settings.ipv4, ipv6 = ?settings.ipv6, dns = ?settings.dns, mtu = settings.mtu, "VPN packet tunnel ready");
        if let Err(error) = self
            .tunnel
            .online(generation, settings, Arc::new(CoreSink(io.clone())))
        {
            let _ = io.error(&error.to_string());
            return;
        }
        if let Err(error) = io
            .pre_tun_config()
            .and_then(|_| io.pre_route_config())
            .and_then(|_| io.connected())
        {
            tracing::error!(%error, "could not activate packet tunnel");
            self.tunnel.offline(generation);
        }
    }
    fn stop(&self) {
        let mut state = self.state.lock().unwrap();
        state.io = None;
        self.tunnel.offline(state.generation);
    }
    fn set_disconnect(&self) {
        self.stop();
    }
    fn send(&self, packet: &[u8]) -> bool {
        let state = self.state.lock().unwrap();
        state.io.is_some() && self.tunnel.receive(state.generation, packet)
    }
    fn info(&self) -> ExternalTunInfo {
        self.state.lock().unwrap().info.clone()
    }
    fn apply_push_update(&self, _options: &str) {
        if let Some(io) = self.state.lock().unwrap().io.as_ref() {
            let _ = io.error("PUSH_UPDATE requires reconnecting with fresh tunnel settings");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(name: &str, error: bool) -> Event {
        Event {
            name: name.into(),
            error,
            fatal: error,
            info: "secret-auth-token".into(),
        }
    }

    fn empty_status() -> Status {
        Status {
            status: String::new(),
            message: "secret-auth-token".into(),
        }
    }

    #[test]
    fn auth_failure_survives_disconnected_and_empty_status() {
        let diagnostics = Diagnostics::default();
        diagnostics.record(&event("AUTH_FAILED", true));
        diagnostics.record(&event("DISCONNECTED", false));
        let message = diagnostics.session_end(&empty_status());
        assert!(message.contains("AUTH_FAILED"));
        assert!(message.contains("server rejected authentication"));
        assert!(!message.contains("secret-auth-token"));
    }

    #[test]
    fn timeout_distinguishes_no_reply_from_incomplete_handshake() {
        let diagnostics = Diagnostics::default();
        diagnostics.record(&event("WAIT", false));
        diagnostics.record(&event("CONNECTION_TIMEOUT", true));
        assert!(
            diagnostics
                .session_end(&empty_status())
                .contains("no reply received")
        );
        diagnostics.record(&event("CONNECTING", false));
        diagnostics.record(&event("CONNECTION_TIMEOUT", true));
        assert!(
            diagnostics
                .session_end(&empty_status())
                .contains("server replied")
        );
    }

    #[test]
    fn successful_reconnect_clears_previous_failure() {
        let diagnostics = Diagnostics::default();
        diagnostics.record(&event("NETWORK_UNREACHABLE", true));
        diagnostics.record(&event("CONNECTED", false));
        diagnostics.record(&event("DISCONNECTED", false));
        assert!(
            !diagnostics
                .session_end(&empty_status())
                .contains("NETWORK_UNREACHABLE")
        );
    }

    #[test]
    fn transport_diagnostics_accept_only_endpoint_and_protocol() {
        assert!(transport_contact("Contacting 127.0.0.1:1194 via UDPv4\n").is_some());
        assert!(transport_contact("Contacting [::1]:1194 via TCPv6").is_some());
        assert!(transport_contact("Contacting user:password via UDPv4").is_none());
        assert!(
            transport_contact("Contacting 127.0.0.1:1194 via UDPv4\nAUTH_TOKEN secret").is_none()
        );
        assert_eq!(event_code("AUTH_FAILED secret"), "OPENVPN_ERROR");
        assert!(
            !Diagnostics::default()
                .session_end(&empty_status())
                .contains("secret-auth-token")
        );
    }
}
