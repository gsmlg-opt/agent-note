//! Only fixed, safe descriptions cross the status/log boundary.
use serde::Serialize;
use std::{fmt, io};
use tokio_tungstenite::tungstenite::{error::TlsError, Error as WsError};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SyncDiagnostic {
    pub timestamp: i64,
    pub level: String,
    pub stage: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub(super) struct Failure {
    pub stage: &'static str,
    pub code: String,
    pub message: String,
}
impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Failure {}
impl Failure {
    pub fn new(stage: &'static str, code: &str, message: &str) -> Self {
        Self {
            stage,
            code: code.into(),
            message: message.into(),
        }
    }
    pub fn event(&self, level: &str) -> SyncDiagnostic {
        SyncDiagnostic {
            timestamp: chrono::Utc::now().timestamp_millis(),
            level: level.into(),
            stage: self.stage.into(),
            code: self.code.clone(),
            message: self.message.clone(),
        }
    }
    pub fn io(error: &io::Error, stage: &'static str) -> Self {
        if let Some(failure) = error.get_ref().and_then(|e| e.downcast_ref::<Self>()) {
            return failure.clone();
        }
        if let Some(error) = error
            .get_ref()
            .and_then(|e| e.downcast_ref::<rustls::Error>())
        {
            return Self::tls(error);
        }
        match error.kind() {
            io::ErrorKind::ConnectionRefused => Self::new(stage, "connection_refused", "Connection refused; verify the GaoNote host, port and service listener."),
            io::ErrorKind::TimedOut => Self::new(stage, "connection_timeout", "Connection timed out; verify the address, firewall and service availability."),
            io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe | io::ErrorKind::UnexpectedEof => Self::new(stage, "connection_closed", "Connection closed unexpectedly; verify the receiver and proxy service logs."),
            _ if error.to_string().contains("lookup address") || error.to_string().contains("Name or service not known") => Self::new(stage, "dns_failed", "DNS lookup failed; verify the configured hostname and server DNS resolver."),
            _ => Self::new(stage, "connection_failed", "Connection failed; verify the configured address, network and receiver availability."),
        }
    }
    pub fn tls(error: &rustls::Error) -> Self {
        use rustls::{CertificateError as Cert, Error};
        match error {
            Error::InvalidCertificate(Cert::NotValidForName | Cert::NotValidForNameContext { .. }) => Self::new("tls", "tls_hostname", "TLS certificate hostname mismatch; use the hostname covered by the GaoNote certificate."),
            Error::InvalidCertificate(Cert::Expired | Cert::ExpiredContext { .. }) => Self::new("tls", "tls_expired", "TLS certificate has expired; renew the certificate and verify the server clock."),
            Error::InvalidCertificate(Cert::UnknownIssuer) => Self::new("tls", "tls_untrusted_ca", "TLS certificate issuer is untrusted; configure the CA that signs the GaoNote certificate."),
            Error::AlertReceived(_) => Self::new("tls", "tls_peer_rejected", "TLS peer rejected the handshake; verify client certificate identity, validity and the receiver's trusted CA."),
            _ => Self::new("tls", "tls_handshake_failed", "TLS handshake failed; verify CA trust, certificate validity, hostname and mTLS client identity."),
        }
    }
    pub fn websocket(error: &WsError) -> Self {
        match error {
            WsError::Http(response) => {
                let status = response.status().as_u16();
                let hint = match status {
                    401 | 403 => "check the GaoNote receiver logs for the rejection reason.",
                    404 => "verify the WebSocket path and reverse proxy route for /note-sync/websocket.",
                    301 | 302 | 307 | 308 => "use the final WebSocket endpoint; verify ws/wss and the reverse proxy route.",
                    400 | 426 => "verify the WebSocket upgrade proxy configuration and Phoenix vsn=2.0.0.",
                    500..=599 => "verify GaoNote and reverse proxy service health and logs.",
                    _ => "verify the GaoNote WebSocket endpoint and receiver logs.",
                };
                Self::new("websocket", &format!("websocket_http_{status}"), &format!("WebSocket handshake rejected with HTTP {status}; {hint}"))
            }
            WsError::Io(error) => Self::io(error, "websocket"),
            WsError::Tls(TlsError::Rustls(error)) => Self::tls(error),
            WsError::Tls(_) => Self::new("tls", "tls_handshake_failed", "TLS handshake failed; verify the configured hostname and certificates."),
            _ => Self::new("websocket", "websocket_handshake_failed", "WebSocket handshake failed; verify the socket path, protocol and reverse proxy upgrade support."),
        }
    }
    pub fn safe(error: &anyhow::Error) -> Self {
        for cause in error.chain() {
            if let Some(failure) = cause.downcast_ref::<Self>() {
                return failure.clone();
            }
        }
        // These contexts are authored locally. Never forward source text or peer payloads.
        let message = error.to_string();
        for safe in [
            "cannot read sync CA certificate",
            "invalid sync CA certificate",
            "sync CA trust store is empty",
            "cannot read sync client certificate",
            "invalid sync client certificate",
            "sync client certificate chain is empty",
            "cannot read sync client private key",
            "invalid sync private key",
            "sync private key is missing",
            "sync certificate and private key do not match",
        ] {
            if message == safe {
                return Self::new(
                    "tls",
                    "tls_configuration",
                    &format!("{safe}; verify certificate files mounted on the Agent Note server."),
                );
            }
        }
        for safe in [
            "cannot read SSH identity file",
            "cannot read SSH known hosts file",
        ] {
            if message == safe {
                return Self::new(
                    "ssh",
                    "ssh_configuration",
                    &format!("{safe}; verify SSH files mounted on the Agent Note server."),
                );
            }
        }
        if message == "Peer identity changed; operator must restore registered GaoNote identity" {
            return Self::new("phoenix", "peer_identity_changed", "Peer identity changed; restore the registered GaoNote identity before retrying. The pinned peer identity remains enforced.");
        }
        let session = match message.as_str() {
            "sync socket closed" => Some(("websocket", "socket_closed", "GaoNote closed the WebSocket connection; verify receiver and proxy logs. Pending notes remain queued.")),
            "sync socket read failed" => Some(("websocket", "socket_read_failed", "WebSocket read failed; verify receiver availability and proxy connection timeouts.")),
            "sync socket write failed" => Some(("websocket", "socket_write_failed", "WebSocket write failed; verify receiver availability and network connectivity.")),
            "sync socket write timed out" | "sync reply write timed out" | "sync rejection write timed out" => Some(("websocket", "socket_write_timeout", "WebSocket write timed out; verify receiver responsiveness and network connectivity.")),
            "sync heartbeat reply timed out" | "sync heartbeat write timed out" => Some(("phoenix", "heartbeat_timeout", "Phoenix heartbeat timed out; verify GaoNote responsiveness and proxy idle timeouts.")),
            "sync Phoenix reply timed out" => Some(("phoenix", "phoenix_reply_timeout", "Phoenix channel reply timed out; verify receiver health and server logs.")),
            "sync Phoenix channel closed" => Some(("phoenix", "phoenix_channel_closed", "GaoNote closed the Phoenix sync channel; verify registered node, transport authorization and receiver logs.")),
            "sync inbound transfer stalled" => Some(("sync", "transfer_stalled", "Incoming note transfer stalled; verify receiver health and network connectivity. Pending notes remain queued.")),
            _ => None,
        };
        if let Some((stage, code, message)) = session {
            return Self::new(stage, code, message);
        }
        if message.contains("selected sync connection profile") {
            return Self::new(
                "config",
                "profile_unavailable",
                "Selected sync connection profile is unavailable; add it in System settings.",
            );
        }
        if message.contains("startup timed out") {
            return Self::new("websocket", "startup_timeout", "Connection startup timed out; verify the network, SSH gateway and GaoNote endpoint.");
        }
        if message.contains("Phoenix")
            || message.contains("peer hello")
            || message.contains("channel request")
        {
            return Self::new("phoenix", "phoenix_protocol", "Phoenix channel negotiation failed; verify the receiver protocol, registered node and authorized selectors.");
        }
        Self::new("sync", "sync_failed", "Synchronization failed; verify receiver health and retry. Pending notes remain queued.")
    }
    pub fn ssh(stderr: &[u8], exit: Option<i32>) -> Self {
        let text = String::from_utf8_lossy(stderr).to_ascii_lowercase();
        let (code, message) = if text.contains("host key verification failed")
            || text.contains("remote host identification has changed")
        {
            ("ssh_host_key", "SSH host key verification failed; verify the gateway key in the configured known hosts file.")
        } else if text.contains("permission denied")
            || text.contains("load key")
            || text.contains("sign_and_send_pubkey")
        {
            ("ssh_authentication", "SSH public key authentication failed; verify the SSH user, identity file and authorized key on the gateway.")
        } else if text.contains("could not resolve hostname")
            || text.contains("name or service not known")
        {
            ("ssh_dns", "SSH hostname lookup failed; verify the gateway or forwarding target hostname and DNS resolver.")
        } else if text.contains("administratively prohibited")
            || text.contains("stdio forwarding failed")
            || text.contains("open failed")
        {
            ("ssh_forwarding", "SSH forwarding failed; verify gateway forwarding permissions and the GaoNote host/port reachable from the gateway.")
        } else if text.contains("connection refused") {
            (
                "ssh_connection_refused",
                "SSH connection refused; verify the gateway host, port and SSH service.",
            )
        } else if text.contains("timed out") || text.contains("timeout") {
            (
                "ssh_timeout",
                "SSH connection timed out; verify gateway reachability and firewall rules.",
            )
        } else {
            ("ssh_failed", "SSH transport closed; verify gateway authentication, host key and forwarding settings.")
        };
        let mut failure = Self::new("ssh", code, message);
        if let Some(exit) = exit {
            failure
                .message
                .push_str(&format!(" SSH exit status: {exit}."));
        }
        failure
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authored_session_reasons_are_actionable_without_raw_sources() {
        for (context, code) in [
            (
                "Peer identity changed; operator must restore registered GaoNote identity",
                "peer_identity_changed",
            ),
            ("sync heartbeat reply timed out", "heartbeat_timeout"),
            ("sync socket closed", "socket_closed"),
            ("sync socket read failed", "socket_read_failed"),
            ("sync socket write failed", "socket_write_failed"),
            ("sync Phoenix reply timed out", "phoenix_reply_timeout"),
            ("sync inbound transfer stalled", "transfer_stalled"),
        ] {
            let error =
                anyhow::anyhow!("header-secret /private-path?token=credential").context(context);
            let safe = Failure::safe(&error);
            assert_eq!(safe.code, code);
            assert!(!safe.message.contains("secret"));
        }
    }
    #[test]
    fn network_and_tls_causes_are_sanitized_and_distinguishable() {
        for (error, code) in [
            (
                io::Error::new(io::ErrorKind::ConnectionRefused, "private-secret"),
                "connection_refused",
            ),
            (
                io::Error::new(io::ErrorKind::TimedOut, "private-secret"),
                "connection_timeout",
            ),
            (
                io::Error::other("failed to lookup address information: private-secret"),
                "dns_failed",
            ),
        ] {
            let safe = Failure::io(&error, "tcp");
            assert_eq!(safe.code, code);
            assert!(!safe.message.contains("secret"));
        }
        for (error, code) in [
            (rustls::CertificateError::UnknownIssuer, "tls_untrusted_ca"),
            (rustls::CertificateError::Expired, "tls_expired"),
            (rustls::CertificateError::NotValidForName, "tls_hostname"),
        ] {
            assert_eq!(
                Failure::tls(&rustls::Error::InvalidCertificate(error)).code,
                code
            );
        }
    }
}
