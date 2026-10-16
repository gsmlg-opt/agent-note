use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;

use crate::api::{
    self, SyncConnectionMode, SyncConnectionSummary, SyncConnectionUpdate, SyncSshUpdate,
};

const MAX_BLOB_BYTES: u64 = 536_870_912;

#[derive(Clone, Copy, PartialEq)]
enum CaMode {
    Keep,
    Native,
    Path,
}

#[derive(Clone, PartialEq)]
struct ConnectionDraft {
    mode: SyncConnectionMode,
    ssh_host: String,
    ssh_port: String,
    ssh_username: String,
    ssh_identity_file: String,
    ssh_known_hosts_file: String,
    id: String,
    url: String,
    client_cert: String,
    client_key: String,
    ca_cert: String,
    ca_mode: CaMode,
    max_blob_bytes: String,
    existing: Option<SyncConnectionSummary>,
}

impl Default for ConnectionDraft {
    fn default() -> Self {
        Self {
            mode: SyncConnectionMode::Mtls,
            ssh_host: String::new(),
            ssh_port: "22".into(),
            ssh_username: String::new(),
            ssh_identity_file: String::new(),
            ssh_known_hosts_file: String::new(),
            id: "gao_note".into(),
            url: "wss://admin.gsmlg.org/note-sync/websocket?vsn=2.0.0".into(),
            client_cert: String::new(),
            client_key: String::new(),
            ca_cert: String::new(),
            ca_mode: CaMode::Native,
            max_blob_bytes: MAX_BLOB_BYTES.to_string(),
            existing: None,
        }
    }
}

impl ConnectionDraft {
    fn edit(summary: SyncConnectionSummary) -> Self {
        let ssh = summary.ssh.as_ref();
        Self {
            mode: summary.mode,
            ssh_host: ssh.map(|item| item.host.clone()).unwrap_or_default(),
            ssh_port: ssh
                .map(|item| item.port.to_string())
                .unwrap_or_else(|| "22".into()),
            ssh_username: ssh.map(|item| item.username.clone()).unwrap_or_default(),
            id: summary.id.clone(),
            url: summary.url.clone(),
            max_blob_bytes: summary.max_blob_bytes.to_string(),
            ca_mode: CaMode::Keep,
            existing: Some(summary),
            ..Self::default()
        }
    }

    fn uses_tls(&self) -> bool {
        self.mode == SyncConnectionMode::Mtls
            || (self.mode == SyncConnectionMode::Direct && self.url.trim().starts_with("wss://"))
    }

    fn request(&self, existing_ids: &[String]) -> Result<SyncConnectionUpdate, String> {
        let id = self.id.trim();
        if id.is_empty() || id.len() > 128 {
            return Err("Connection ID must contain 1 to 128 bytes.".into());
        }
        if self.existing.is_none() && existing_ids.iter().any(|existing| existing == id) {
            return Err(
                "This connection ID already exists. Cancel and use Edit connection to update it."
                    .into(),
            );
        }
        let url = self.url.trim();
        match self.mode {
            SyncConnectionMode::Mtls if !url.starts_with("wss://") => {
                return Err(
                    "mTLS requires a wss:// GaoNote WebSocket address with vsn=2.0.0.".into(),
                );
            }
            SyncConnectionMode::Ssh if !url.starts_with("ws://") => {
                return Err("SSH requires a ws:// GaoNote address reachable from the SSH host, with vsn=2.0.0.".into());
            }
            SyncConnectionMode::Direct
                if !url.starts_with("ws://") && !url.starts_with("wss://") =>
            {
                return Err(
                    "Enter a ws:// or wss:// GaoNote WebSocket address with vsn=2.0.0.".into(),
                );
            }
            _ => {}
        }
        let max_blob_bytes = self
            .max_blob_bytes
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|bytes| (1..=MAX_BLOB_BYTES).contains(bytes))
            .ok_or("Per-note transfer limit must be 1 to 536870912 bytes (512 MiB).")?;
        let nonempty = |value: &str| {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_string())
        };
        let (client_cert, client_key) = if self.mode == SyncConnectionMode::Mtls {
            let existing = self
                .existing
                .as_ref()
                .filter(|item| item.mode == SyncConnectionMode::Mtls);
            let client_cert = nonempty(&self.client_cert);
            let client_key = nonempty(&self.client_key);
            if client_cert.is_none() && !existing.is_some_and(|item| item.has_client_cert) {
                return Err("Enter the client certificate file path on the server.".into());
            }
            if client_key.is_none() && !existing.is_some_and(|item| item.has_client_key) {
                return Err("Enter the client private key file path on the server.".into());
            }
            (client_cert, client_key)
        } else {
            (None, None)
        };
        let ca_cert = if self.uses_tls() {
            match self.ca_mode {
                CaMode::Keep => None,
                CaMode::Native => Some(String::new()),
                CaMode::Path => Some(
                    nonempty(&self.ca_cert)
                        .ok_or("Enter the CA certificate file path on the server.")?,
                ),
            }
        } else {
            None
        };
        let ssh = if self.mode == SyncConnectionMode::Ssh {
            let host = nonempty(&self.ssh_host).ok_or("Enter the SSH host.")?;
            let username = nonempty(&self.ssh_username).ok_or("Enter the SSH username.")?;
            let port = self
                .ssh_port
                .trim()
                .parse::<u16>()
                .ok()
                .filter(|port| *port > 0)
                .ok_or("SSH port must be 1 to 65535.")?;
            let existing = self
                .existing
                .as_ref()
                .filter(|item| item.mode == SyncConnectionMode::Ssh)
                .and_then(|item| item.ssh.as_ref());
            let identity_file = nonempty(&self.ssh_identity_file);
            let known_hosts_file = nonempty(&self.ssh_known_hosts_file);
            if identity_file.is_none() && !existing.is_some_and(|item| item.has_identity_file) {
                return Err("Enter the SSH private key file path on the server.".into());
            }
            if known_hosts_file.is_none() && !existing.is_some_and(|item| item.has_known_hosts_file)
            {
                return Err("Enter the SSH known_hosts file path on the server.".into());
            }
            Some(SyncSshUpdate {
                host,
                port,
                username,
                identity_file,
                known_hosts_file,
            })
        } else {
            None
        };
        Ok(SyncConnectionUpdate {
            mode: self.mode,
            ssh,
            id: self
                .existing
                .as_ref()
                .map_or_else(|| id.to_string(), |item| item.id.clone()),
            url: self.url.trim().into(),
            client_cert,
            client_key,
            ca_cert,
            max_blob_bytes,
        })
    }
}

#[derive(Properties, PartialEq)]
pub struct SyncConnectionEditorProps {
    pub profile_id: Option<String>,
    pub existing_ids: Vec<String>,
    pub onsaved: Callback<SyncConnectionSummary>,
    pub oncancel: Callback<MouseEvent>,
}

#[function_component(SyncConnectionEditor)]
pub fn sync_connection_editor(props: &SyncConnectionEditorProps) -> Html {
    let draft = use_state(ConnectionDraft::default);
    let loading = use_state(|| props.profile_id.is_some());
    let saving = use_state(|| false);
    let error = use_state(|| None::<String>);
    let ready = use_state(|| props.profile_id.is_none());
    let active = use_mut_ref(|| true);
    {
        let draft = draft.clone();
        let loading = loading.clone();
        let ready = ready.clone();
        let error = error.clone();
        let active = active.clone();
        use_effect_with(props.profile_id.clone(), move |id| {
            if let Some(id) = id.clone() {
                let active = active.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let result = api::get_sync_connections().await;
                    if !*active.borrow() {
                        return;
                    }
                    match result {
                        Ok(items) => match items.into_iter().find(|item| item.id == id) {
                            Some(item) => {
                                draft.set(ConnectionDraft::edit(item));
                                ready.set(true);
                            }
                            None => {
                                error.set(Some("This connection is no longer available.".into()))
                            }
                        },
                        Err(message) => error.set(Some(message)),
                    }
                    loading.set(false);
                });
            }
            move || *active.borrow_mut() = false
        });
    }
    let on_save = {
        let draft = draft.clone();
        let error = error.clone();
        let saving = saving.clone();
        let onsaved = props.onsaved.clone();
        let active = active.clone();
        let existing_ids = props.existing_ids.clone();
        Callback::from(move |_| {
            let request = match draft.request(&existing_ids) {
                Ok(request) => request,
                Err(message) => {
                    error.set(Some(message));
                    return;
                }
            };
            saving.set(true);
            error.set(None);
            let saving = saving.clone();
            let error = error.clone();
            let onsaved = onsaved.clone();
            let active = active.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let result = api::update_sync_connection(&request).await;
                if !*active.borrow() {
                    return;
                }
                match result {
                    Ok(summary) => onsaved.emit(summary),
                    Err(message) => {
                        error.set(Some(message));
                        saving.set(false);
                    }
                }
            });
        })
    };
    let disabled = *saving || *loading || !*ready;
    let field = |label: &'static str,
                 field: &'static str,
                 value: String,
                 placeholder: &'static str| {
        let onchange = {
            let draft = draft.clone();
            Callback::from(move |event: InputEvent| {
                let input: HtmlInputElement = event.target_unchecked_into();
                let mut next = (*draft).clone();
                match field {
                    "id" => next.id = input.value(),
                    "url" => next.url = input.value(),
                    "cert" => next.client_cert = input.value(),
                    "key" => next.client_key = input.value(),
                    "ca" => next.ca_cert = input.value(),
                    "ssh_host" => next.ssh_host = input.value(),
                    "ssh_port" => next.ssh_port = input.value(),
                    "ssh_username" => next.ssh_username = input.value(),
                    "ssh_identity" => next.ssh_identity_file = input.value(),
                    "ssh_known_hosts" => next.ssh_known_hosts_file = input.value(),
                    _ => next.max_blob_bytes = input.value(),
                }
                draft.set(next);
            })
        };
        html! {
            <label class="field">
                <span>{ label }</span>
                <input type={if field == "limit" || field == "ssh_port" { "number" } else { "text" }} class="input"
                    value={value} placeholder={placeholder} oninput={onchange}
                    disabled={disabled || (field == "id" && props.profile_id.is_some())}
                    autocomplete="off" spellcheck="false" />
            </label>
        }
    };
    let on_mode = {
        let draft = draft.clone();
        Callback::from(move |event: Event| {
            let input: HtmlSelectElement = event.target_unchecked_into();
            let mut next = (*draft).clone();
            next.mode = match input.value().as_str() {
                "direct" => SyncConnectionMode::Direct,
                "ssh" => SyncConnectionMode::Ssh,
                _ => SyncConnectionMode::Mtls,
            };
            draft.set(next);
        })
    };
    let on_ca_mode = {
        let draft = draft.clone();
        Callback::from(move |event: Event| {
            let input: HtmlSelectElement = event.target_unchecked_into();
            let mut next = (*draft).clone();
            next.ca_mode = match input.value().as_str() {
                "native" => CaMode::Native,
                "path" => CaMode::Path,
                _ => CaMode::Keep,
            };
            draft.set(next);
        })
    };
    html! {
        <div class="sync-connection-editor" role="region" aria-labelledby="connection-editor-title">
            <h4 id="connection-editor-title">{ if props.profile_id.is_some() { "Edit connection" } else { "Add connection" } }</h4>
            <p class="system-score-help">{ "Choose how Agent Note connects to your GaoNote server. Enter its WebSocket address below." }</p>
            if *loading { <p role="status">{ "Loading connection..." }</p> }
            { field("Connection ID", "id", draft.id.clone(), "gao_note") }
            <label class="field">
                <span>{ "Connection method" }</span>
                <select class="select" disabled={disabled} onchange={on_mode}>
                    <option value="direct" selected={draft.mode == SyncConnectionMode::Direct}>{ "Direct connection" }</option>
                    <option value="ssh" selected={draft.mode == SyncConnectionMode::Ssh}>{ "SSH tunnel" }</option>
                    <option value="mtls" selected={draft.mode == SyncConnectionMode::Mtls}>{ "mTLS" }</option>
                </select>
            </label>
            { field("GaoNote WebSocket address", "url", draft.url.clone(), if draft.mode == SyncConnectionMode::Ssh { "ws://127.0.0.1:4664/note-sync/websocket?vsn=2.0.0" } else { "wss://your-gaonote.example/note-sync/websocket?vsn=2.0.0" }) }
            if draft.mode == SyncConnectionMode::Ssh {
                <p class="system-score-help">{ "Enter GaoNote's ws:// address as reached from the SSH host. Agent Note automatically opens the SSH tunnel and verifies the host using the known_hosts file." }</p>
                { field("SSH host", "ssh_host", draft.ssh_host.clone(), "ssh.example.com") }
                { field("SSH port", "ssh_port", draft.ssh_port.clone(), "22") }
                { field("SSH username", "ssh_username", draft.ssh_username.clone(), "notes") }
                { field("SSH private key path", "ssh_identity", draft.ssh_identity_file.clone(), "/certs/ssh-key") }
                { field("SSH known_hosts path", "ssh_known_hosts", draft.ssh_known_hosts_file.clone(), "/certs/known_hosts") }
            }
            if draft.mode == SyncConnectionMode::Mtls {
                { field("Client certificate path", "cert", draft.client_cert.clone(), "/certs/client.pem") }
                { field("Client private key path", "key", draft.client_key.clone(), "/certs/client-key.pem") }
            }
            if draft.mode != SyncConnectionMode::Direct || draft.uses_tls() {
                <p class="system-score-help">{ "File paths refer to the Agent Note server/container. Provision files there first; private key contents stay on the server." }</p>
            }
            if props.profile_id.is_some() {
                <p class="system-score-help">{ "The connection ID stays fixed. Existing credential paths are hidden; leave a path blank to keep its saved value for the same connection method." }</p>
            }
            if draft.uses_tls() {
                <label class="field">
                    <span>{ "Server CA trust" }</span>
                    <select class="select" disabled={disabled} onchange={on_ca_mode}>
                        if props.profile_id.is_some() {
                            <option value="keep" selected={draft.ca_mode == CaMode::Keep}>{ if draft.existing.as_ref().is_some_and(|item| item.has_ca_cert) { "Keep existing custom CA" } else { "Keep existing native trust roots" } }</option>
                        }
                        <option value="native" selected={draft.ca_mode == CaMode::Native}>{ "Use native trust roots" }</option>
                        <option value="path" selected={draft.ca_mode == CaMode::Path}>{ "Use a CA certificate file on the server" }</option>
                    </select>
                </label>
                if draft.ca_mode == CaMode::Path {
                    { field("CA certificate path", "ca", draft.ca_cert.clone(), "/certs/ca.pem") }
                }
            }
            { field("Maximum note transfer size (bytes)", "limit", draft.max_blob_bytes.clone(), "536870912") }
            <p class="system-score-help">{ "Includes note event JSON and all attachments. Allowed range: 1 to 536870912 bytes (512 MiB)." }</p>
            <p class="system-score-help">{ "Save connection applies connection details immediately, including to active sync. Use Save settings afterwards to enable sync or apply selected connection and rules." }</p>
            if let Some(message) = &*error {
                <p role="alert" class="system-score-error">{ message }</p>
            }
            <div class="system-section-actions">
                <button type="button" class="btn btn-outline" disabled={*saving} onclick={props.oncancel.clone()}>{ "Cancel" }</button>
                <button type="button" class="btn btn-primary" disabled={disabled} onclick={on_save}>{ if *saving { "Saving connection..." } else { "Save connection" } }</button>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary() -> SyncConnectionSummary {
        SyncConnectionSummary {
            mode: SyncConnectionMode::Mtls,
            ssh: None,
            id: "paired-peer".into(),
            url: "wss://example.test/socket?vsn=2.0.0".into(),
            max_blob_bytes: 1234567,
            has_client_cert: true,
            has_client_key: true,
            has_ca_cert: true,
        }
    }

    fn ssh_summary() -> SyncConnectionSummary {
        let mut item = summary();
        item.mode = SyncConnectionMode::Ssh;
        item.url = "ws://127.0.0.1:4664/note-sync/websocket?vsn=2.0.0".into();
        item.has_client_cert = false;
        item.has_client_key = false;
        item.has_ca_cert = false;
        item.ssh = Some(api::SyncSshSummary {
            host: "ssh.example.test".into(),
            port: 2222,
            username: "notes".into(),
            has_identity_file: true,
            has_known_hosts_file: true,
        });
        item
    }

    fn new_ssh() -> ConnectionDraft {
        ConnectionDraft {
            mode: SyncConnectionMode::Ssh,
            url: "ws://127.0.0.1:4664/note-sync/websocket?vsn=2.0.0".into(),
            ssh_host: "ssh.example.test".into(),
            ssh_username: "notes".into(),
            ssh_identity_file: "/certs/ssh-key".into(),
            ssh_known_hosts_file: "/certs/known_hosts".into(),
            ..ConnectionDraft::default()
        }
    }

    #[test]
    fn direct_accepts_arbitrary_ws_without_client_credentials() {
        let draft = ConnectionDraft {
            mode: SyncConnectionMode::Direct,
            url: "ws://dev.example.test:4664/note-sync/websocket?vsn=2.0.0".into(),
            ..ConnectionDraft::default()
        };
        let request = draft.request(&[]).unwrap();
        assert_eq!(request.url, draft.url);
        assert_eq!(request.mode, SyncConnectionMode::Direct);
        assert_eq!(request.client_cert, None);
        assert_eq!(request.client_key, None);
        assert_eq!(request.ca_cert, None);
        assert_eq!(request.ssh, None);
    }

    #[test]
    fn direct_wss_allows_custom_ca_without_client_credentials() {
        let draft = ConnectionDraft {
            mode: SyncConnectionMode::Direct,
            ca_mode: CaMode::Path,
            ca_cert: "/certs/ca.pem".into(),
            ..ConnectionDraft::default()
        };
        let request = draft.request(&[]).unwrap();
        assert_eq!(request.ca_cert.as_deref(), Some("/certs/ca.pem"));
        assert_eq!(request.client_cert, None);
        assert_eq!(request.client_key, None);
    }

    #[test]
    fn switching_modes_preserves_address_and_omits_hidden_credentials() {
        let mut draft = ConnectionDraft::edit(summary());
        draft.client_cert = "/certs/client.pem".into();
        draft.client_key = "/certs/client-key.pem".into();
        draft.ca_cert = "/certs/ca.pem".into();
        draft.ca_mode = CaMode::Path;
        let original_url = draft.url.clone();
        draft.mode = SyncConnectionMode::Direct;
        let body = serde_json::to_value(draft.request(&[]).unwrap()).unwrap();
        assert_eq!(body["url"], original_url);
        assert!(body.get("client_cert").is_none());
        assert!(body.get("client_key").is_none());
        assert!(body.get("ssh").is_none());
        draft.url = "ws://dev.test/socket?vsn=2.0.0".into();
        let body = serde_json::to_value(draft.request(&[]).unwrap()).unwrap();
        assert!(body.get("ca_cert").is_none());
        let mut ssh = new_ssh();
        ssh.client_cert = draft.client_cert;
        ssh.client_key = draft.client_key;
        ssh.ca_mode = CaMode::Path;
        ssh.ca_cert = draft.ca_cert;
        let body = serde_json::to_value(ssh.request(&[]).unwrap()).unwrap();
        assert!(body.get("client_cert").is_none());
        assert!(body.get("client_key").is_none());
        assert!(body.get("ca_cert").is_none());
        ssh.mode = SyncConnectionMode::Direct;
        let body = serde_json::to_value(ssh.request(&[]).unwrap()).unwrap();
        assert!(body.get("ssh").is_none());
    }

    #[test]
    fn ssh_request_requires_host_user_and_both_server_paths() {
        let mut draft = new_ssh();
        let request = draft.request(&[]).unwrap();
        let ssh = request.ssh.unwrap();
        assert_eq!(ssh.host, "ssh.example.test");
        assert_eq!(ssh.port, 22);
        assert_eq!(ssh.username, "notes");
        assert_eq!(ssh.identity_file.as_deref(), Some("/certs/ssh-key"));
        assert_eq!(ssh.known_hosts_file.as_deref(), Some("/certs/known_hosts"));
        draft.ssh_host.clear();
        assert!(draft.request(&[]).unwrap_err().contains("SSH host"));
        draft.ssh_host = "ssh.example.test".into();
        draft.ssh_username.clear();
        assert!(draft.request(&[]).unwrap_err().contains("SSH username"));
        draft.ssh_username = "notes".into();
        draft.ssh_identity_file.clear();
        assert!(draft.request(&[]).unwrap_err().contains("private key"));
        draft.ssh_identity_file = "/certs/key".into();
        draft.ssh_known_hosts_file.clear();
        assert!(draft.request(&[]).unwrap_err().contains("known_hosts"));
    }

    #[test]
    fn editing_ssh_loads_metadata_and_preserves_write_only_paths() {
        let draft = ConnectionDraft::edit(ssh_summary());
        assert_eq!(draft.mode, SyncConnectionMode::Ssh);
        assert_eq!(draft.ssh_host, "ssh.example.test");
        assert_eq!(draft.ssh_port, "2222");
        assert_eq!(draft.ssh_username, "notes");
        assert!(draft.ssh_identity_file.is_empty());
        assert!(draft.ssh_known_hosts_file.is_empty());
        let body = serde_json::to_value(draft.request(&[]).unwrap()).unwrap();
        assert_eq!(body["ssh"]["port"], 2222);
        assert!(body["ssh"].get("identity_file").is_none());
        assert!(body["ssh"].get("known_hosts_file").is_none());
    }

    #[test]
    fn ssh_edit_rotates_one_path_and_requires_missing_saved_paths() {
        let mut draft = ConnectionDraft::edit(ssh_summary());
        draft.ssh_identity_file = " /certs/new-ssh-key ".into();
        let request = draft.request(&[]).unwrap().ssh.unwrap();
        assert_eq!(request.identity_file.as_deref(), Some("/certs/new-ssh-key"));
        assert_eq!(request.known_hosts_file, None);
        draft
            .existing
            .as_mut()
            .unwrap()
            .ssh
            .as_mut()
            .unwrap()
            .has_known_hosts_file = false;
        assert!(draft.request(&[]).unwrap_err().contains("known_hosts"));
        draft.ssh_known_hosts_file = "/certs/known_hosts".into();
        assert!(draft.request(&[]).is_ok());
    }

    #[test]
    fn ssh_summary_decodes_safe_metadata_for_editing() {
        let item: SyncConnectionSummary = serde_json::from_value(serde_json::json!({
            "id": "remote", "mode": "ssh", "url": "ws://127.0.0.1:4664/socket?vsn=2.0.0",
            "max_blob_bytes": 1234567, "has_client_cert": false, "has_client_key": false,
            "has_ca_cert": false, "ssh": { "host": "ssh.example.test", "port": 2222,
                "username": "notes", "has_identity_file": true, "has_known_hosts_file": true }
        }))
        .unwrap();
        let request = ConnectionDraft::edit(item).request(&[]).unwrap();
        let body = serde_json::to_value(request).unwrap();
        assert_eq!(body["mode"], "ssh");
        assert_eq!(body["ssh"]["host"], "ssh.example.test");
        assert_eq!(body["ssh"]["username"], "notes");
        assert_eq!(body["ssh"]["port"], 2222);
        assert!(body["ssh"].get("identity_file").is_none());
        assert!(body["ssh"].get("known_hosts_file").is_none());
    }

    #[test]
    fn mode_changes_require_credentials_for_the_new_method() {
        let mut from_ssh = ConnectionDraft::edit(ssh_summary());
        from_ssh.mode = SyncConnectionMode::Mtls;
        from_ssh.url = "wss://other.test/socket?vsn=2.0.0".into();
        assert!(from_ssh.request(&[]).unwrap_err().contains("certificate"));
        let mut from_mtls = ConnectionDraft::edit(summary());
        from_mtls.mode = SyncConnectionMode::Ssh;
        from_mtls.url = "ws://127.0.0.1:4664/socket?vsn=2.0.0".into();
        from_mtls.ssh_host = "ssh.test".into();
        from_mtls.ssh_username = "notes".into();
        assert!(from_mtls.request(&[]).unwrap_err().contains("private key"));
    }

    #[test]
    fn ssh_ports_are_validated_without_defaulting_invalid_input() {
        let mut draft = new_ssh();
        for invalid in ["0", "65536", "-1", "22.5", ""] {
            draft.ssh_port = invalid.into();
            assert!(draft.request(&[]).unwrap_err().contains("SSH port"));
        }
        draft.ssh_port = "65535".into();
        assert_eq!(draft.request(&[]).unwrap().ssh.unwrap().port, 65535);
        draft.url = "wss://remote.test/socket?vsn=2.0.0".into();
        assert!(draft.request(&[]).unwrap_err().contains("ws://"));
    }

    #[test]
    fn legacy_summary_defaults_to_mtls_without_ssh_metadata() {
        let summary: SyncConnectionSummary = serde_json::from_value(serde_json::json!({
            "id": "old", "url": "wss://old.test/socket?vsn=2.0.0", "max_blob_bytes": 123,
            "has_client_cert": true, "has_client_key": true, "has_ca_cert": false
        }))
        .unwrap();
        assert_eq!(summary.mode, SyncConnectionMode::Mtls);
        assert_eq!(summary.ssh, None);
    }

    #[test]
    fn add_rejects_an_existing_id_without_blocking_its_edit() {
        let mut draft = ConnectionDraft::default();
        draft.client_cert = "/certs/client.pem".into();
        draft.client_key = "/certs/client-key.pem".into();
        let existing_ids = vec!["gao_note".into(), "paired-peer".into()];
        assert!(draft
            .request(&existing_ids)
            .unwrap_err()
            .contains("Edit connection"));
        draft.id = "new-peer".into();
        assert!(draft.request(&existing_ids).is_ok());
        assert!(ConnectionDraft::edit(summary())
            .request(&existing_ids)
            .is_ok());
    }

    #[test]
    fn new_connection_requires_both_server_credential_paths() {
        let mut draft = ConnectionDraft::default();
        assert!(draft.request(&[]).unwrap_err().contains("certificate"));
        draft.client_cert = "/certs/client.pem".into();
        assert!(draft.request(&[]).unwrap_err().contains("private key"));
        draft.client_key = "/certs/client-key.pem".into();
        let request = draft.request(&[]).unwrap();
        assert_eq!(request.id, "gao_note");
        assert_eq!(request.client_cert.as_deref(), Some("/certs/client.pem"));
        assert_eq!(request.client_key.as_deref(), Some("/certs/client-key.pem"));
        assert_eq!(request.ca_cert.as_deref(), Some(""));
    }

    #[test]
    fn editing_keeps_identity_limit_and_omits_hidden_credentials() {
        let mut draft = ConnectionDraft::edit(summary());
        draft.id = "accidental-replacement".into();
        let body = serde_json::to_value(draft.request(&[]).unwrap()).unwrap();
        assert_eq!(body["id"], "paired-peer");
        assert_eq!(body["max_blob_bytes"], 1234567);
        assert!(body.get("client_cert").is_none());
        assert!(body.get("client_key").is_none());
        assert!(body.get("ca_cert").is_none());
    }

    #[test]
    fn changing_one_credential_does_not_clear_the_other() {
        let mut draft = ConnectionDraft::edit(summary());
        draft.client_cert = " /certs/replacement.pem ".into();
        let request = draft.request(&[]).unwrap();
        assert_eq!(
            request.client_cert.as_deref(),
            Some("/certs/replacement.pem")
        );
        assert_eq!(request.client_key, None);
    }

    #[test]
    fn custom_ca_requires_path_and_native_roots_clear_only_explicitly() {
        let mut draft = ConnectionDraft::edit(summary());
        assert_eq!(draft.request(&[]).unwrap().ca_cert, None);
        draft.ca_mode = CaMode::Path;
        assert!(draft.request(&[]).unwrap_err().contains("CA certificate"));
        draft.ca_cert = " /certs/custom-ca.pem ".into();
        assert_eq!(
            draft.request(&[]).unwrap().ca_cert.as_deref(),
            Some("/certs/custom-ca.pem")
        );
        draft.ca_mode = CaMode::Native;
        assert_eq!(draft.request(&[]).unwrap().ca_cert.as_deref(), Some(""));
    }

    #[test]
    fn rejects_invalid_blob_limits_and_insecure_urls() {
        let mut draft = ConnectionDraft::edit(summary());
        for invalid in ["0", "536870913", "1.5", "-1", ""] {
            draft.max_blob_bytes = invalid.into();
            assert!(draft.request(&[]).is_err(), "{invalid}");
        }
        draft.max_blob_bytes = "536870912".into();
        assert!(draft.request(&[]).is_ok());
        draft.url = "ws://example.test/socket?vsn=2.0.0".into();
        assert!(draft.request(&[]).is_err());
    }
}
