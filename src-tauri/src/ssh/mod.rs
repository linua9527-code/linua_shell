use crate::proxy::ProxyConfig;
use anyhow::Result;
use russh::*;
use russh_keys::*;
use russh_sftp::client::{Config as SftpClientConfig, SftpSession};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Preferred host-key algorithms advertised to the server, ordered from most to
/// least preferred.  RSA variants (including the legacy `ssh-rsa` / SHA-1) are
/// included so that older servers that only offer RSA host keys are still
/// reachable.  The `openssl` feature on `russh` / `russh-keys` must be enabled
/// for the RSA entries to have any effect.
pub static PREFERRED_HOST_KEY_ALGOS: &[russh_keys::key::Name] = &[
    russh_keys::key::ED25519,
    russh_keys::key::ECDSA_SHA2_NISTP256,
    russh_keys::key::ECDSA_SHA2_NISTP521,
    russh_keys::key::RSA_SHA2_256,
    russh_keys::key::RSA_SHA2_512,
    russh_keys::key::SSH_RSA,
];

const BASH_VERSION_PROBE: &str = r#"printf '__RSHELL_BASH_VERSION__%s' "${BASH_VERSION-}""#;
const BASH_VERSION_MARKER: &str = "__RSHELL_BASH_VERSION__";
const BASH_SHELL_INTEGRATION_PREFIX: &str = r#" stty echo; __rshell_report_cwd(){ local p=${PWD//%/%25}; p=${p// /%20}; p=${p//#/%23}; p=${p//\?/%3F}; printf '\033]7;file://%s%s\033\\' "${HOSTNAME:-localhost}" "$p"; }; "#;
const BASH_SHELL_INTEGRATION_SUFFIX: &str = "printf '\\r\\033[2K'\n";

// Tor onion-service circuits can take several seconds to build, especially
// on the first connection. Keep the timeout long enough for SOCKS5 and SSH
// handshakes while still bounding stalled connections.
const CONNECTION_TIMEOUT_SECS: u64 = 30;
const SFTP_UPLOAD_BUFFER_SIZE: usize = 256 * 1024;

pub(crate) fn sftp_session_config() -> SftpClientConfig {
    SftpClientConfig {
        request_timeout_secs: 120,
        ..SftpClientConfig::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct BashVersion {
    pub(crate) major: u32,
    pub(crate) minor: u32,
}

pub(crate) fn bash_version_from_probe(output: &str) -> Option<BashVersion> {
    let version = output.rsplit_once(BASH_VERSION_MARKER)?.1.trim();
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some(BashVersion { major, minor })
}

pub(crate) fn bash_shell_integration_command(version: BashVersion) -> Vec<u8> {
    let prompt_command = if version >= (BashVersion { major: 5, minor: 1 }) {
        r#"if declare -p PROMPT_COMMAND &>/dev/null; then PROMPT_COMMAND=("${PROMPT_COMMAND[@]}" __rshell_report_cwd); else PROMPT_COMMAND=(__rshell_report_cwd); fi; "#
    } else {
        r#"if [[ -n ${PROMPT_COMMAND-} ]]; then PROMPT_COMMAND+=$'\n__rshell_report_cwd'; else PROMPT_COMMAND=__rshell_report_cwd; fi; "#
    };

    format!(
        "{}{}{}",
        BASH_SHELL_INTEGRATION_PREFIX, prompt_command, BASH_SHELL_INTEGRATION_SUFFIX
    )
    .into_bytes()
}

/// Compression algorithms to advertise, ordered so zlib is preferred over none.
///
/// Order matters: russh negotiates the first algorithm that the server also
/// lists, so zlib must come before none for compression to actually take
/// effect. `zlib@openssh.com` covers servers using OpenSSH's "delayed"
/// compression. Requires russh's `flate2` feature, which is enabled by default.
pub fn compression_preferences(enabled: bool) -> &'static [russh::compression::Name] {
    if enabled {
        &[
            russh::compression::ZLIB,
            russh::compression::ZLIB_LEGACY,
            russh::compression::NONE,
        ]
    } else {
        &[russh::compression::NONE]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: AuthMethod,
    /// Enable zlib compression negotiation (default: true, matching the UI).
    pub compression: bool,
    /// Keepalive interval in seconds. `None` disables keepalive.
    pub keepalive_interval: Option<u64>,
    /// Max missed keepalive replies before the connection is closed.
    pub keepalive_max: Option<u32>,
    /// Optional HTTP/SOCKS proxy tunnel. `None` connects directly.
    pub proxy: Option<ProxyConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AuthMethod {
    Password {
        password: String,
    },
    PublicKey {
        key_path: String,
        passphrase: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct SshSession {
    pub id: String,
    pub config: SshConfig,
    pub connected: bool,
}

pub struct SshClient {
    session: Option<Arc<client::Handle<Client>>>,
    config: Option<SshConfig>,
}

// PTY session handle for interactive shell
pub struct PtySession {
    pub input_tx: mpsc::Sender<Vec<u8>>,
    pub output_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<Vec<u8>>>>,
    pub channel_id: ChannelId,
    /// Sender for resize requests (cols, rows) — forwarded to the SSH channel
    pub resize_tx: mpsc::Sender<(u32, u32)>,
    /// Cancellation token — cancelled when this session is torn down.
    /// The WebSocket reader task should select on this to stop promptly.
    pub cancel: CancellationToken,
}

pub struct Client;

#[async_trait::async_trait]
impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true) // In production, verify the server key
    }

    async fn disconnected(
        &mut self,
        reason: client::DisconnectReason<Self::Error>,
    ) -> Result<(), Self::Error> {
        match reason {
            client::DisconnectReason::ReceivedDisconnect(info) => {
                tracing::warn!(
                    reason_code = ?info.reason_code,
                    message = %info.message,
                    language = %info.lang_tag,
                    "SSH server sent a disconnect"
                );
                Ok(())
            }
            client::DisconnectReason::Error(error) => {
                tracing::error!(error = %error, "SSH transport disconnected with an error");
                Err(error)
            }
        }
    }
}

impl SshClient {
    pub fn new() -> Self {
        Self {
            session: None,
            config: None,
        }
    }

    pub async fn connect(&mut self, config: &SshConfig) -> Result<()> {
        let keepalive_interval = config.keepalive_interval.map(Duration::from_secs);

        let ssh_config = client::Config {
            preferred: russh::Preferred {
                key: std::borrow::Cow::Borrowed(PREFERRED_HOST_KEY_ALGOS),
                compression: std::borrow::Cow::Borrowed(compression_preferences(
                    config.compression,
                )),
                ..russh::Preferred::DEFAULT
            },
            // Send a keepalive on the user-configured interval. After the
            // configured number of missed replies russh closes the connection,
            // preventing the server from silently dropping idle sessions.
            keepalive_interval,
            keepalive_max: config.keepalive_max.unwrap_or(3) as usize,
            ..client::Config::default()
        };

        let connection_timeout = Duration::from_secs(CONNECTION_TIMEOUT_SECS);

        let mut ssh_session = if let Some(proxy) = &config.proxy {
            // Tunnel through the proxy first, then hand the established stream
            // to russh so the SSH handshake runs over the tunnel.
            let stream = crate::proxy::connect_via_proxy(
                proxy,
                &config.host,
                config.port,
                connection_timeout,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Proxy connection failed: {e}"))?;
            tokio::time::timeout(
                connection_timeout,
                client::connect_stream(Arc::new(ssh_config), stream, Client),
            )
            .await
            .map_err(|_| anyhow::anyhow!(
                "Connection timed out after {CONNECTION_TIMEOUT_SECS} seconds. Please check the host address and network connectivity."
            ))?
            .map_err(|e| anyhow::anyhow!("Failed to connect to {}:{}: {}", config.host, config.port, e))?
        } else {
            tokio::time::timeout(
                connection_timeout,
                client::connect(Arc::new(ssh_config), (&config.host[..], config.port), Client),
            )
            .await
            .map_err(|_| anyhow::anyhow!(
                "Connection timed out after {CONNECTION_TIMEOUT_SECS} seconds. Please check the host address and network connectivity."
            ))?
            .map_err(|e| anyhow::anyhow!("Failed to connect to {}:{}: {}", config.host, config.port, e))?
        };

        let authenticated = match &config.auth_method {
            AuthMethod::Password { password } => ssh_session
                .authenticate_password(&config.username, password)
                .await
                .map_err(|e| anyhow::anyhow!("Password authentication failed: {}", e))?,
            AuthMethod::PublicKey {
                key_path,
                passphrase,
            } => {
                // Expand tilde in path — use dirs::home_dir() for cross-platform
                // support (HOME is not set on Windows; USERPROFILE is used instead).
                let expanded_path = if key_path.starts_with("~/") || key_path.starts_with("~\\") {
                    if let Some(home) = dirs::home_dir() {
                        let home_str = home.to_string_lossy();
                        key_path.replacen('~', &home_str, 1)
                    } else {
                        key_path.clone()
                    }
                } else {
                    key_path.clone()
                };

                // Check if file exists
                if !std::path::Path::new(&expanded_path).exists() {
                    return Err(anyhow::anyhow!(
                        "SSH key file not found: {}. Please check the file path and try again.",
                        key_path
                    ));
                }

                // Read the key file and normalise CRLF line endings so that keys
                // created or edited on Windows (which use \r\n) are parsed correctly
                // by russh-keys' PEM / OpenSSH decoder.
                let key_content = std::fs::read_to_string(&expanded_path).map_err(|e| {
                    anyhow::anyhow!("Failed to read SSH key file {}: {}", key_path, e)
                })?;
                let key_content = key_content.replace("\r\n", "\n");

                // decode_secret_key takes the key *content* as a &str.
                let key = decode_secret_key(&key_content, passphrase.as_deref())
                    .map_err(|e| {
                        if e.to_string().contains("encrypted") || e.to_string().contains("passphrase") {
                            anyhow::anyhow!(
                                "Failed to decrypt SSH key. The key may be encrypted. Please provide the correct passphrase."
                            )
                        } else {
                            anyhow::anyhow!(
                                "Failed to load SSH key from {}: {}. Ensure the file is a valid SSH private key (RSA, Ed25519, or ECDSA).",
                                key_path, e
                            )
                        }
                    })?;

                ssh_session
                    .authenticate_publickey(&config.username, Arc::new(key))
                    .await
                    .map_err(|e| anyhow::anyhow!("Public key authentication failed: {}. The key may not be authorized on the server.", e))?
            }
        };

        if !authenticated {
            return Err(anyhow::anyhow!(
                "Authentication failed. Please check your credentials and try again."
            ));
        }

        self.session = Some(Arc::new(ssh_session));
        self.config = Some(config.clone());
        Ok(())
    }

    // Changed to &self instead of &mut self to allow concurrent access
    pub async fn execute_command(&self, command: &str) -> Result<String> {
        let (output, _, code) = self.execute_command_result(command).await?;
        match code {
            Some(0) => Ok(output),
            None if !output.is_empty() => Ok(output),
            _ => Err(anyhow::anyhow!("Command failed with code: {:?}", code)),
        }
    }

    pub async fn execute_command_checked(&self, command: &str) -> Result<String> {
        let (output, error_output, code) = self.execute_command_result(command).await?;
        match code {
            Some(0) => Ok(output),
            _ => Err(anyhow::anyhow!(
                "Remote command exited with {:?}: {}",
                code,
                if error_output.trim().is_empty() {
                    output.trim()
                } else {
                    error_output.trim()
                }
            )),
        }
    }

    async fn execute_command_result(&self, command: &str) -> Result<(String, String, Option<u32>)> {
        if let Some(session) = &self.session {
            let mut channel = session.channel_open_session().await?;
            channel.exec(true, command).await?;

            let mut output = String::new();
            let mut error_output = String::new();
            let mut code = None;
            let mut eof_received = false;
            let mut server_closed = false;

            loop {
                let msg = channel.wait().await;
                match msg {
                    Some(ChannelMsg::Data { ref data }) => {
                        output.push_str(&String::from_utf8_lossy(data));
                    }
                    Some(ChannelMsg::ExtendedData { ref data, .. }) => {
                        error_output.push_str(&String::from_utf8_lossy(data));
                    }
                    Some(ChannelMsg::ExitStatus { exit_status }) => {
                        code = Some(exit_status);
                        if eof_received {
                            break;
                        }
                    }
                    Some(ChannelMsg::Eof) => {
                        eof_received = true;
                        if code.is_some() {
                            break;
                        }
                    }
                    Some(ChannelMsg::Close) => {
                        server_closed = true;
                        break;
                    }
                    None => {
                        server_closed = true;
                        break;
                    }
                    _ => {}
                }
            }

            // Send SSH_MSG_CHANNEL_CLOSE if the server hasn't already closed the channel.
            // Without this, russh's session keeps the channel in its internal map until
            // the session is torn down, causing per-poll memory growth.
            if !server_closed {
                let _ = channel.close().await;
            }

            Ok((output, error_output, code))
        } else {
            Err(anyhow::anyhow!("Not connected"))
        }
    }

    pub async fn disconnect(&mut self) -> Result<()> {
        self.config = None;
        if let Some(session) = self.session.take() {
            // Try to unwrap Arc, if we're the only owner
            match Arc::try_unwrap(session) {
                Ok(session) => {
                    session
                        .disconnect(Disconnect::ByApplication, "", "English")
                        .await?;
                }
                Err(arc_session) => {
                    // Other references exist, just drop our reference
                    drop(arc_session);
                }
            }
        }
        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        self.session.is_some()
    }

    /// Create a persistent PTY shell session (like ttyd)
    /// This enables interactive commands like vim, less, more, top, etc.
    pub async fn create_pty_session(&self, cols: u32, rows: u32) -> Result<PtySession> {
        if let Some(session) = &self.session {
            let bash_version = tokio::time::timeout(
                Duration::from_secs(2),
                self.execute_command(BASH_VERSION_PROBE),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .and_then(|output| bash_version_from_probe(&output));

            // Open a new SSH channel
            let mut channel = session.channel_open_session().await?;
            let bash_terminal_modes = [(Pty::ECHO, 0), (Pty::ECHONL, 0)];
            let terminal_modes = if bash_version.is_some() {
                bash_terminal_modes.as_slice()
            } else {
                &[]
            };

            // Request PTY with terminal type and dimensions
            // Similar to ttyd's approach: xterm-256color terminal
            channel
                .request_pty(
                    true,             // want_reply
                    "xterm-256color", // terminal type (like ttyd)
                    cols,             // columns
                    rows,             // rows
                    0,                // pixel_width (not used)
                    0,                // pixel_height (not used)
                    terminal_modes,
                )
                .await?;

            // Start interactive shell
            channel.request_shell(true).await?;

            // Create channels for bidirectional communication (like ttyd's pty_buf)
            // Increased capacity for better buffering during fast input
            let (input_tx, mut input_rx) = mpsc::channel::<Vec<u8>>(1000); // Increased from 100
            let (output_tx, output_rx) = mpsc::channel::<Vec<u8>>(128); // Bounded: back-pressure to SSH window

            let channel_id = channel.id();
            let ssh_session = Arc::clone(session);

            // Clone channel for input task
            let mut input_channel = channel.make_writer();
            if let Some(version) = bash_version {
                let integration_command = bash_shell_integration_command(version);
                input_channel.write_all(&integration_command).await?;
                input_channel.flush().await?;
            }

            // Create a channel for resize requests
            let (resize_tx, mut resize_rx) = mpsc::channel::<(u32, u32)>(16);

            // Spawn task to handle input (frontend → SSH)
            // This is similar to ttyd's pty_write and INPUT command handling
            // Key: immediate write + flush for responsiveness
            tokio::spawn(async move {
                let mut writer = input_channel;
                while let Some(data) = input_rx.recv().await {
                    // Write data immediately
                    if let Err(e) = writer.write_all(&data).await {
                        eprintln!("[PTY] Failed to send data to SSH: {}", e);
                        break;
                    }
                    // Critical: flush immediately after write (like ttyd)
                    // This ensures data is sent to PTY without buffering delay
                    if let Err(e) = writer.flush().await {
                        eprintln!("[PTY] Failed to flush data to SSH: {}", e);
                        break;
                    }
                }
            });

            // Spawn task to handle output (SSH → frontend) AND resize requests.
            // The channel must stay in this task because `wait()` requires `&mut self`,
            // but we also need `window_change()` which only requires `&self`.
            // We use `tokio::select!` to multiplex between output reading and resize.
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        msg = channel.wait() => {
                            match msg {
                                Some(ChannelMsg::Data { data }) => {
                                    if output_tx.send(data.to_vec()).await.is_err() {
                                        break;
                                    }
                                }
                                Some(ChannelMsg::ExtendedData { data, .. }) => {
                                    // stderr data (also send to output)
                                    if output_tx.send(data.to_vec()).await.is_err() {
                                        break;
                                    }
                                }
                                Some(ChannelMsg::Eof) => {
                                    tracing::warn!(
                                        "PTY channel received EOF for channel {:?}",
                                        channel_id
                                    );
                                    break;
                                }
                                Some(ChannelMsg::Close) => {
                                    tracing::warn!(
                                        "PTY channel received CLOSE for channel {:?}",
                                        channel_id
                                    );
                                    break;
                                }
                                None => {
                                    tracing::error!(
                                        channel = ?channel_id,
                                        ssh_transport_closed = ssh_session.is_closed(),
                                        "PTY channel message stream ended"
                                    );
                                    break;
                                }
                                Some(ChannelMsg::ExitStatus { exit_status }) => {
                                    eprintln!("[PTY] Process exited with status: {}", exit_status);
                                }
                                _ => {}
                            }
                        }
                        resize = resize_rx.recv() => {
                            match resize {
                                Some((cols, rows)) => {
                                    if let Err(e) = channel.window_change(cols, rows, 0, 0).await {
                                        eprintln!("[PTY] Failed to send window change: {}", e);
                                    } else {
                                        eprintln!("[PTY] Window changed to {}x{}", cols, rows);
                                    }
                                }
                                None => {
                                    // resize channel closed, session is being torn down
                                    break;
                                }
                            }
                        }
                    }
                }
            });

            Ok(PtySession {
                input_tx,
                output_rx: Arc::new(tokio::sync::Mutex::new(output_rx)),
                channel_id,
                resize_tx,
                cancel: CancellationToken::new(),
            })
        } else {
            Err(anyhow::anyhow!("Not connected"))
        }
    }

    pub(crate) async fn open_sftp_session(&self) -> Result<SftpSession> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Not connected"))?;
        let channel = session.channel_open_session().await?;
        channel.request_subsystem(true, "sftp").await?;
        Ok(SftpSession::new_with_config(channel.into_stream(), sftp_session_config()).await?)
    }

    pub async fn download_file(&self, remote_path: &str, local_path: &str) -> Result<u64> {
        if self.session.is_some() {
            let sftp = self.open_sftp_session().await?;

            // Open remote file for reading
            let mut remote_file = sftp.open(remote_path).await?;

            // Read file content
            let mut buffer = Vec::new();
            let mut temp_buf = vec![0u8; 8192];
            let mut total_bytes = 0u64;

            loop {
                let n = remote_file.read(&mut temp_buf).await?;
                if n == 0 {
                    break;
                }
                buffer.extend_from_slice(&temp_buf[..n]);
                total_bytes += n as u64;
            }

            // Write to local file
            tokio::fs::write(local_path, buffer).await?;

            Ok(total_bytes)
        } else {
            Err(anyhow::anyhow!("Not connected"))
        }
    }

    pub async fn download_file_to_memory(&self, remote_path: &str) -> Result<Vec<u8>> {
        if self.session.is_some() {
            let sftp = self.open_sftp_session().await?;

            // Open remote file for reading
            let mut remote_file = sftp.open(remote_path).await?;

            // Read file content
            let mut buffer = Vec::new();
            let mut temp_buf = vec![0u8; 8192];

            loop {
                let n = remote_file.read(&mut temp_buf).await?;
                if n == 0 {
                    break;
                }
                buffer.extend_from_slice(&temp_buf[..n]);
            }

            Ok(buffer)
        } else {
            Err(anyhow::anyhow!("Not connected"))
        }
    }

    pub async fn upload_file(&self, local_path: &str, remote_path: &str) -> Result<u64> {
        self.upload_file_with_progress(local_path, remote_path, |_| {})
            .await
    }

    pub async fn upload_file_with_progress<F>(
        &self,
        local_path: &str,
        remote_path: &str,
        mut on_progress: F,
    ) -> Result<u64>
    where
        F: FnMut(u64) + Send,
    {
        let config = self
            .config
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("SSH connection configuration is unavailable"))?;
        let mut transfer_client = SshClient::new();
        tracing::info!("Opening dedicated SSH transport for file upload");
        transfer_client.connect(config).await?;

        let result = transfer_client
            .upload_file_with_progress_on_session(local_path, remote_path, &mut on_progress)
            .await;
        let disconnect_result = transfer_client.disconnect().await;

        match result {
            Ok(bytes) => {
                disconnect_result?;
                Ok(bytes)
            }
            Err(error) => Err(error),
        }
    }

    async fn upload_file_with_progress_on_session<F>(
        &self,
        local_path: &str,
        remote_path: &str,
        on_progress: F,
    ) -> Result<u64>
    where
        F: FnMut(u64) + Send,
    {
        let mut local_file = tokio::fs::File::open(local_path).await?;
        self.upload_from_reader(&mut local_file, remote_path, on_progress)
            .await
    }

    pub async fn upload_file_from_bytes(&self, data: &[u8], remote_path: &str) -> Result<u64> {
        let config = self
            .config
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("SSH connection configuration is unavailable"))?;
        let mut transfer_client = SshClient::new();
        transfer_client.connect(config).await?;

        let result = transfer_client
            .upload_bytes_on_session(data, remote_path)
            .await;
        let disconnect_result = transfer_client.disconnect().await;

        match result {
            Ok(bytes) => {
                disconnect_result?;
                Ok(bytes)
            }
            Err(error) => Err(error),
        }
    }

    async fn upload_bytes_on_session(&self, data: &[u8], remote_path: &str) -> Result<u64> {
        let mut source = data;
        self.upload_from_reader(&mut source, remote_path, |_| {})
            .await
    }

    async fn upload_from_reader<R: AsyncRead + Unpin>(
        &self,
        source: &mut R,
        remote_path: &str,
        mut on_progress: impl FnMut(u64) + Send,
    ) -> Result<u64> {
        let sftp = self
            .open_sftp_session()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to open SFTP channel: {e}"))?;
        let mut remote_file = sftp
            .create(remote_path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create remote file '{}': {e}", remote_path))?;
        let mut buffer = vec![0u8; SFTP_UPLOAD_BUFFER_SIZE];
        let mut total_bytes = 0u64;
        let mut source_chunks = 0u64;
        let mut last_logged_bytes = 0u64;
        let started_at = Instant::now();

        tracing::info!("SSH SFTP upload started");

        loop {
            let count = source.read(&mut buffer).await.map_err(|e| {
                anyhow::anyhow!(
                    "Failed to read local file at byte {total_bytes} after {} ms: {e}",
                    started_at.elapsed().as_millis()
                )
            })?;
            if count == 0 {
                break;
            }
            remote_file.write_all(&buffer[..count]).await.map_err(|e| {
                anyhow::anyhow!(
                    "SFTP write failed at byte {total_bytes} after {} ms: {e}",
                    started_at.elapsed().as_millis()
                )
            })?;
            total_bytes += count as u64;
            source_chunks += 1;
            on_progress(total_bytes);

            if total_bytes - last_logged_bytes >= 1024 * 1024 {
                tracing::info!(
                    bytes_transferred = total_bytes,
                    source_chunks,
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    "SSH SFTP upload progress"
                );
                last_logged_bytes = total_bytes;
            }
        }

        remote_file
            .flush()
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "SFTP flush failed after {total_bytes} bytes and {source_chunks} source chunks in {} ms: {e}",
                    started_at.elapsed().as_millis()
                )
            })?;
        remote_file.shutdown().await.map_err(|e| {
            anyhow::anyhow!(
                "Failed to close remote file after {total_bytes} bytes and {source_chunks} source chunks in {} ms: {e}",
                started_at.elapsed().as_millis()
            )
        })?;
        sftp.close().await.map_err(|e| {
            anyhow::anyhow!(
                "Failed to close SFTP channel after {total_bytes} bytes and {source_chunks} source chunks in {} ms: {e}",
                started_at.elapsed().as_millis()
            )
        })?;
        tracing::info!(
            bytes_transferred = total_bytes,
            source_chunks,
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            "SSH SFTP upload completed"
        );
        Ok(total_bytes)
    }
}

#[cfg(test)]
mod tests;
