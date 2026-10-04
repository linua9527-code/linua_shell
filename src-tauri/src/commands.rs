use crate::connection_manager::ConnectionManager;
use crate::ftp_client::FtpConfig;
use crate::os_detect::{self, OsInfo};
use crate::proxy::{ProxyConfig, ProxyType};
use crate::sftp_client::{FileEntry, FileEntryType, SftpAuthMethod, SftpConfig};
use crate::ssh::{AuthMethod, SshConfig};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::Emitter;
use tauri::State;
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

#[derive(Debug, Serialize, Deserialize)]
pub struct ConnectRequest {
    pub connection_id: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: String,
    pub password: Option<String>,
    pub key_path: Option<String>,
    pub passphrase: Option<String>,
    /// Advanced SSH options — `Option` so legacy callers that omit them keep
    /// the previous defaults (compression on, keepalive 60 s / 3).
    pub compression: Option<bool>,
    pub keepalive_enabled: Option<bool>,
    pub keepalive_interval: Option<u64>,
    pub keepalive_max: Option<u32>,
    /// Proxy options — ignored when `proxy_type` is "none" or missing.
    pub proxy_type: Option<String>,
    pub proxy_host: Option<String>,
    pub proxy_port: Option<u16>,
    pub proxy_username: Option<String>,
    pub proxy_password: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CommandResponse {
    pub success: bool,
    pub output: Option<String>,
    pub error: Option<String>,
}

#[tauri::command]
pub async fn ssh_connect(
    request: ConnectRequest,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<CommandResponse, String> {
    let proxy = build_proxy(&request)?;

    // Keepalive defaults match the connection dialog UI: enabled at 60 s / 3.
    let keepalive_enabled = request.keepalive_enabled.unwrap_or(true);
    let keepalive_interval = if keepalive_enabled {
        Some(request.keepalive_interval.unwrap_or(60))
    } else {
        None
    };
    let keepalive_max = if keepalive_enabled {
        Some(request.keepalive_max.unwrap_or(3))
    } else {
        None
    };

    let auth_method = match request.auth_method.as_str() {
        "password" => AuthMethod::Password {
            password: request.password.ok_or("Password required")?,
        },
        "publickey" => AuthMethod::PublicKey {
            key_path: request.key_path.ok_or("Key path required")?,
            passphrase: request.passphrase,
        },
        _ => return Err("Invalid auth method".to_string()),
    };

    let config = SshConfig {
        host: request.host,
        port: request.port,
        username: request.username,
        auth_method,
        compression: request.compression.unwrap_or(true),
        keepalive_interval,
        keepalive_max,
        proxy,
    };

    match state
        .create_connection(request.connection_id.clone(), config)
        .await
    {
        Ok(_) => Ok(CommandResponse {
            success: true,
            output: Some(format!("Connected: {}", request.connection_id)),
            error: None,
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            output: None,
            error: Some(e.to_string()),
        }),
    }
}

/// Map the proxy request fields into a `ProxyConfig`, or `None` when the
/// connection should go direct.
fn build_proxy(request: &ConnectRequest) -> Result<Option<ProxyConfig>, String> {
    match request.proxy_type.as_deref() {
        None | Some("none") | Some("") => Ok(None),
        Some(kind) => {
            let host = request
                .proxy_host
                .clone()
                .filter(|h| !h.trim().is_empty())
                .ok_or("Proxy host is required")?;
            let proxy_type = match kind {
                "http" => ProxyType::Http,
                "socks4" => ProxyType::Socks4,
                "socks5" => ProxyType::Socks5,
                other => return Err(format!("Invalid proxy type: {other}")),
            };
            Ok(Some(ProxyConfig {
                proxy_type,
                host,
                port: request.proxy_port.unwrap_or(8080),
                username: request.proxy_username.clone(),
                password: request.proxy_password.clone(),
            }))
        }
    }
}

#[tauri::command]
pub async fn ssh_cancel_connect(
    connection_id: String,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<CommandResponse, String> {
    if state.cancel_pending_connection(&connection_id).await {
        Ok(CommandResponse {
            success: true,
            output: Some("Connection cancelled".to_string()),
            error: None,
        })
    } else {
        Ok(CommandResponse {
            success: false,
            output: None,
            error: Some("No pending connection to cancel".to_string()),
        })
    }
}

#[tauri::command]
pub async fn ssh_disconnect(
    connection_id: String,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<CommandResponse, String> {
    match state.close_connection(&connection_id).await {
        Ok(_) => Ok(CommandResponse {
            success: true,
            output: Some("Disconnected".to_string()),
            error: None,
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            output: None,
            error: Some(e.to_string()),
        }),
    }
}

#[tauri::command]
pub async fn ssh_execute_command(
    connection_id: String,
    command: String,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<CommandResponse, String> {
    let connection = state
        .get_connection(&connection_id)
        .await
        .ok_or("Connection not found")?;

    let client = connection.read().await;

    // Transform interactive commands to batch mode
    let transformed_command = transform_interactive_command(&command);

    match client.execute_command(&transformed_command).await {
        Ok(output) => Ok(CommandResponse {
            success: true,
            output: Some(output),
            error: None,
        }),
        Err(e) => {
            // Check if it's an interactive command that failed
            let error_msg = if is_interactive_command(&command) {
                format!("{}\n\nNote: Interactive commands like '{}' may not work in this terminal. Try using batch mode alternatives.",
                    e,
                    get_command_name(&command))
            } else {
                e.to_string()
            };

            Ok(CommandResponse {
                success: false,
                output: None,
                error: Some(error_msg),
            })
        }
    }
}

// Helper function to transform interactive commands to batch mode
fn transform_interactive_command(command: &str) -> String {
    let cmd = command.trim();

    // Handle 'top' - convert to batch mode with 1 iteration
    if cmd == "top" || cmd.starts_with("top ") {
        return format!("{} -bn1", cmd);
    }

    // Handle 'htop' - suggest alternative
    if cmd == "htop" || cmd.starts_with("htop ") {
        return "top -bn1".to_string();
    }

    // Return original command if no transformation needed
    command.to_string()
}

// Helper function to check if a command is interactive
fn is_interactive_command(command: &str) -> bool {
    let cmd_name = get_command_name(command);
    matches!(
        cmd_name.as_str(),
        "top"
            | "htop"
            | "vim"
            | "vi"
            | "nano"
            | "emacs"
            | "less"
            | "more"
            | "man"
            | "tmux"
            | "screen"
    )
}

// Helper function to extract command name
fn get_command_name(command: &str) -> String {
    command.split_whitespace().next().unwrap_or("").to_string()
}

/// Get or detect OS info for a connection (cached after first call).
///
/// Concurrent callers for the same connection share a single in-flight
/// detection via `OnceCell`; only the first caller runs `detect_os`.
async fn get_os_info(
    connection_id: &str,
    client: &crate::ssh::SshClient,
    state: &Arc<ConnectionManager>,
) -> OsInfo {
    state
        .os_info_cache()
        .get_or_init(connection_id, || async {
            let info = os_detect::detect_os(client).await;
            tracing::info!(
                "Detected OS for {}: {} ({})",
                connection_id,
                info.pretty_name,
                info.id
            );
            info
        })
        .await
}

#[tauri::command]
pub async fn list_files(
    connection_id: String,
    path: String,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<Vec<FileEntry>, String> {
    let connection = state
        .get_connection(&connection_id)
        .await
        .ok_or("Connection not found")?;

    let client = connection.read().await;
    let os_info = get_os_info(&connection_id, &client, state.inner()).await;
    let command = os_info.list_files_cmd(&path);

    let output = client
        .execute_command(&command)
        .await
        .map_err(|e| e.to_string())?;

    // Parse the `ls -l` output on the backend so the frontend never has to
    // guess the column layout. Supports GNU `--time-style=long-iso` (used when
    // GNU coreutils are detected) and the BusyBox/BSD default layout, plus
    // ACL/SELinux/no-group variants. See `ls_parser` for details.
    let entries = output
        .lines()
        .filter_map(crate::ls_parser::parse_ls_long_line)
        .collect::<Vec<_>>();

    Ok(entries)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileTransferRequest {
    pub connection_id: String,
    pub local_path: String,
    pub remote_path: String,
    pub data: Option<Vec<u8>>, // For upload: file contents
}

#[derive(Debug, Serialize)]
pub struct FileTransferResponse {
    pub success: bool,
    pub bytes_transferred: Option<u64>,
    pub data: Option<Vec<u8>>, // For download: file contents
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileTransferProgress {
    pub transfer_id: String,
    pub connection_id: String,
    pub bytes_transferred: u64,
    pub total_bytes: u64,
}

fn emit_file_transfer_progress(
    app: &tauri::AppHandle,
    transfer_id: Option<&str>,
    connection_id: &str,
    bytes_transferred: u64,
    total_bytes: u64,
) {
    let Some(transfer_id) = transfer_id else {
        return;
    };

    let _ = app.emit(
        "file-transfer-progress",
        FileTransferProgress {
            transfer_id: transfer_id.to_string(),
            connection_id: connection_id.to_string(),
            bytes_transferred,
            total_bytes,
        },
    );
}

/// @deprecated Use `download_remote_file` instead. Kept for backward compatibility.
#[tauri::command]
pub async fn sftp_download_file(
    request: FileTransferRequest,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<FileTransferResponse, String> {
    let connection = state
        .get_connection(&request.connection_id)
        .await
        .ok_or("Connection not found")?;

    let client = connection.read().await;

    // If local_path is empty, download to memory (for browser download)
    if request.local_path.is_empty() {
        match client.download_file_to_memory(&request.remote_path).await {
            Ok(data) => {
                let bytes = data.len() as u64;
                Ok(FileTransferResponse {
                    success: true,
                    bytes_transferred: Some(bytes),
                    data: Some(data),
                    error: None,
                })
            }
            Err(e) => Ok(FileTransferResponse {
                success: false,
                bytes_transferred: None,
                data: None,
                error: Some(e.to_string()),
            }),
        }
    } else {
        // Download to local file
        match client
            .download_file(&request.remote_path, &request.local_path)
            .await
        {
            Ok(bytes) => Ok(FileTransferResponse {
                success: true,
                bytes_transferred: Some(bytes),
                data: None,
                error: None,
            }),
            Err(e) => Ok(FileTransferResponse {
                success: false,
                bytes_transferred: None,
                data: None,
                error: Some(e.to_string()),
            }),
        }
    }
}

/// @deprecated Use `upload_remote_file` instead. Kept for backward compatibility.
#[tauri::command]
pub async fn sftp_upload_file(
    request: FileTransferRequest,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<FileTransferResponse, String> {
    let connection = state
        .get_connection(&request.connection_id)
        .await
        .ok_or("Connection not found")?;

    let client = connection.read().await;

    // If data is provided, write directly; otherwise read from local_path
    let result = if let Some(data) = &request.data {
        client
            .upload_file_from_bytes(data, &request.remote_path)
            .await
    } else {
        client
            .upload_file(&request.local_path, &request.remote_path)
            .await
    };

    match result {
        Ok(bytes) => Ok(FileTransferResponse {
            success: true,
            bytes_transferred: Some(bytes),
            data: None,
            error: None,
        }),
        Err(e) => Ok(FileTransferResponse {
            success: false,
            bytes_transferred: None,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

// File operation commands

/// Escape a path for use inside a POSIX single-quoted shell argument.
/// Single quotes cannot appear inside a single-quoted string, so we end the
/// quote, emit the escaped quote, and reopen the quote: `'` → `'\''`.
fn shell_escape_single_quoted(path: &str) -> String {
    path.replace('\'', "'\\''")
}

#[tauri::command]
pub async fn create_directory(
    connection_id: String,
    path: String,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<bool, String> {
    let connection = state
        .get_connection(&connection_id)
        .await
        .ok_or("Connection not found")?;

    let client = connection.read().await;
    let command = format!("mkdir -p '{}'", shell_escape_single_quoted(&path));

    match client.execute_command(&command).await {
        Ok(_) => Ok(true),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
pub async fn delete_file(
    connection_id: String,
    path: String,
    is_directory: bool,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<bool, String> {
    let connection = state
        .get_connection(&connection_id)
        .await
        .ok_or("Connection not found")?;

    let client = connection.read().await;
    let command = if is_directory {
        format!("rm -rf '{}'", shell_escape_single_quoted(&path))
    } else {
        format!("rm -f '{}'", shell_escape_single_quoted(&path))
    };

    match client.execute_command(&command).await {
        Ok(_) => Ok(true),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
pub async fn rename_file(
    connection_id: String,
    old_path: String,
    new_path: String,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<bool, String> {
    let connection = state
        .get_connection(&connection_id)
        .await
        .ok_or("Connection not found")?;

    let client = connection.read().await;
    let command = format!(
        "mv '{}' '{}'",
        shell_escape_single_quoted(&old_path),
        shell_escape_single_quoted(&new_path)
    );

    match client.execute_command(&command).await {
        Ok(_) => Ok(true),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
pub async fn create_file(
    connection_id: String,
    path: String,
    content: String,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<bool, String> {
    let connection = state
        .get_connection(&connection_id)
        .await
        .ok_or("Connection not found")?;

    let client = connection.read().await;

    // Upload the content as bytes
    match client
        .upload_file_from_bytes(content.as_bytes(), &path)
        .await
    {
        Ok(_) => Ok(true),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
pub async fn read_file_content(
    connection_id: String,
    path: String,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<String, String> {
    let connection = state
        .get_connection(&connection_id)
        .await
        .ok_or("Connection not found")?;

    let client = connection.read().await;
    let command = format!("cat '{}'", path);

    match client.execute_command(&command).await {
        Ok(output) => Ok(output),
        Err(e) => Err(e.to_string()),
    }
}

/// Response for `read_remote_file_base64`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Base64FileResponse {
    pub data: String,
    pub size: u64,
    pub mime_type: String,
}

/// Read a remote file and return its content as base64 with an inferred MIME type.
/// Used for image previews in the embedded file viewer. Refuses files > 20 MB.
#[tauri::command]
pub async fn read_remote_file_base64(
    connection_id: String,
    path: String,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<Base64FileResponse, String> {
    let connection = state
        .get_connection(&connection_id)
        .await
        .ok_or("Connection not found")?;

    let client = connection.read().await;

    // Refuse very large files to avoid memory / performance issues
    let size_cmd = format!(
        "stat -c '%s' '{}' 2>/dev/null || stat -f '%z' '{}'",
        path, path
    );
    let size_str = client
        .execute_command(&size_cmd)
        .await
        .unwrap_or_default()
        .trim()
        .to_string();
    let file_size: u64 = size_str.parse().unwrap_or(0);
    if file_size > 20 * 1024 * 1024 {
        return Err(format!(
            "File too large for preview ({} MB). Download it first to open with your local application.",
            file_size / (1024 * 1024)
        ));
    }

    // Read raw bytes via `cat` then base64-encode on the remote side.
    // `base64 -w0` (GNU) disables line wrapping; on macOS `base64` wraps by default,
    // so we pipe through `tr -d '\n'` as a portable fallback.
    let b64_cmd = format!(
        "base64 -w0 '{}' 2>/dev/null || base64 '{}' | tr -d '\\n'",
        path, path
    );
    let b64_data = client
        .execute_command(&b64_cmd)
        .await
        .map_err(|e| e.to_string())?;

    // Infer MIME type from extension
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "tiff" | "tif" => "image/tiff",
        "avif" => "image/avif",
        _ => "application/octet-stream",
    };

    Ok(Base64FileResponse {
        data: b64_data.trim().to_string(),
        size: file_size,
        mime_type: mime.to_string(),
    })
}

#[tauri::command]
pub async fn copy_file(
    connection_id: String,
    source_path: String,
    dest_path: String,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<bool, String> {
    let connection = state
        .get_connection(&connection_id)
        .await
        .ok_or("Connection not found")?;

    let client = connection.read().await;
    let command = format!("cp -r '{}' '{}'", source_path, dest_path);

    match client.execute_command(&command).await {
        Ok(_) => Ok(true),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
pub async fn list_connections(
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<Vec<String>, String> {
    Ok(state.list_connections().await)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TabCompletionRequest {
    pub connection_id: String,
    pub input: String,
    pub cursor_position: usize,
}

#[derive(Debug, Serialize)]
pub struct TabCompletionResponse {
    pub success: bool,
    pub completions: Vec<String>,
    pub common_prefix: Option<String>,
    pub error: Option<String>,
}

#[tauri::command]
pub async fn ssh_tab_complete(
    connection_id: String,
    input: String,
    cursor_position: usize,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<TabCompletionResponse, String> {
    let connection = state
        .get_connection(&connection_id)
        .await
        .ok_or("Connection not found")?;

    let client = connection.read().await;

    // Extract the word to complete (last word before cursor)
    let text_before_cursor = &input[..cursor_position.min(input.len())];
    let words: Vec<&str> = text_before_cursor.split_whitespace().collect();
    let word_to_complete = words.last().copied().unwrap_or("");

    // Determine completion type
    let is_first_word = words.len() <= 1;

    // Build completion command based on context
    let completion_cmd = if is_first_word {
        // Command completion: use compgen -c for commands
        format!("compgen -c {} 2>/dev/null || echo", word_to_complete)
    } else {
        // File/directory completion: use compgen -f for files
        format!(
            "compgen -f {} 2>/dev/null || ls -1ap {} 2>/dev/null | grep '^{}' || echo",
            word_to_complete,
            if word_to_complete.is_empty() {
                "."
            } else {
                word_to_complete
            },
            word_to_complete
        )
    };

    match client.execute_command(&completion_cmd).await {
        Ok(output) => {
            let completions: Vec<String> = output
                .lines()
                .filter(|s| !s.is_empty() && s.starts_with(word_to_complete))
                .map(|s| s.trim().to_string())
                .take(50) // Limit to 50 completions
                .collect();

            // Find common prefix
            let common_prefix = if completions.len() > 1 {
                find_common_prefix(&completions)
            } else {
                None
            };

            Ok(TabCompletionResponse {
                success: true,
                completions,
                common_prefix,
                error: None,
            })
        }
        Err(e) => Ok(TabCompletionResponse {
            success: false,
            completions: Vec::new(),
            common_prefix: None,
            error: Some(e.to_string()),
        }),
    }
}

// Helper function to find common prefix among strings
fn find_common_prefix(strings: &[String]) -> Option<String> {
    if strings.is_empty() {
        return None;
    }
    if strings.len() == 1 {
        return Some(strings[0].clone());
    }

    let first = &strings[0];
    let mut prefix = String::new();

    for (i, ch) in first.chars().enumerate() {
        if strings.iter().all(|s| s.chars().nth(i) == Some(ch)) {
            prefix.push(ch);
        } else {
            break;
        }
    }

    if prefix.is_empty() || prefix == strings[0] {
        None
    } else {
        Some(prefix)
    }
}

// ========== WebSocket Port ==========

/// Get the dynamically assigned WebSocket port for PTY terminal connections
#[tauri::command]
pub async fn get_websocket_port() -> Result<u16, String> {
    use crate::WEBSOCKET_PORT;
    use std::sync::atomic::Ordering;

    let port = WEBSOCKET_PORT.load(Ordering::SeqCst);
    if port == 0 {
        Err("WebSocket server not yet started".to_string())
    } else {
        Ok(port)
    }
}

// ========== PTY Connection ==========
// PTY terminal I/O now uses WebSocket instead of IPC for better performance
// WebSocket server runs on a dynamically assigned port (9001-9010)
// Use get_websocket_port() command to get the actual port
// See src/websocket_server.rs for implementation

// ========== Standalone SFTP Connection ==========

#[derive(Debug, Deserialize)]
pub struct SftpConnectRequest {
    pub connection_id: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: String,
    pub password: Option<String>,
    pub key_path: Option<String>,
    pub passphrase: Option<String>,
}

#[tauri::command]
pub async fn sftp_connect(
    request: SftpConnectRequest,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<CommandResponse, String> {
    let auth = match request.auth_method.as_str() {
        "password" => SftpAuthMethod::Password {
            password: request.password.unwrap_or_default(),
        },
        "publickey" => SftpAuthMethod::PublicKey {
            key_path: request.key_path.ok_or("Key path required for SFTP")?,
            passphrase: request.passphrase,
        },
        _ => return Err("Invalid SFTP auth method".to_string()),
    };

    let config = SftpConfig {
        host: request.host,
        port: request.port,
        username: request.username,
        auth_method: auth,
    };

    match state
        .create_sftp_connection(request.connection_id.clone(), config)
        .await
    {
        Ok(_) => Ok(CommandResponse {
            success: true,
            output: Some(format!("SFTP connected: {}", request.connection_id)),
            error: None,
        }),
        Err(e) => Err(format!("SFTP connection failed: {}", e)),
    }
}

#[tauri::command]
pub async fn sftp_standalone_disconnect(
    connection_id: String,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<CommandResponse, String> {
    match state.close_sftp_connection(&connection_id).await {
        Ok(_) => Ok(CommandResponse {
            success: true,
            output: Some("SFTP disconnected".to_string()),
            error: None,
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            output: None,
            error: Some(e.to_string()),
        }),
    }
}

// ========== FTP Connection ==========

#[derive(Debug, Deserialize)]
pub struct FtpConnectRequest {
    pub connection_id: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: Option<String>,
    pub ftps_enabled: bool,
    pub anonymous: bool,
}

#[tauri::command]
pub async fn ftp_connect(
    request: FtpConnectRequest,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<CommandResponse, String> {
    tracing::info!(
        "ftp_connect: id={}, host={}:{}, user={}, ftps={}, anon={}",
        request.connection_id,
        request.host,
        request.port,
        request.username,
        request.ftps_enabled,
        request.anonymous
    );

    let config = FtpConfig {
        host: request.host,
        port: request.port,
        username: if request.anonymous {
            "anonymous".to_string()
        } else {
            request.username
        },
        password: if request.anonymous {
            "anonymous@".to_string()
        } else {
            request.password.unwrap_or_default()
        },
        ftps_enabled: request.ftps_enabled,
        anonymous: request.anonymous,
    };

    match state
        .create_ftp_connection(request.connection_id.clone(), config)
        .await
    {
        Ok(_) => Ok(CommandResponse {
            success: true,
            output: Some(format!("FTP connected: {}", request.connection_id)),
            error: None,
        }),
        Err(e) => Err(format!("FTP connection failed: {}", e)),
    }
}

#[tauri::command]
pub async fn ftp_disconnect(
    connection_id: String,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<CommandResponse, String> {
    match state.close_ftp_connection(&connection_id).await {
        Ok(_) => Ok(CommandResponse {
            success: true,
            output: Some("FTP disconnected".to_string()),
            error: None,
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            output: None,
            error: Some(e.to_string()),
        }),
    }
}

// ========== Unified File Operations ==========

#[tauri::command]
pub async fn list_remote_files(
    connection_id: String,
    path: String,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<Vec<FileEntry>, String> {
    let conn_type = state
        .get_connection_type(&connection_id)
        .await
        .ok_or_else(|| format!("No file connection found for '{}'", connection_id))?;

    match conn_type.as_str() {
        "SFTP" => {
            let sftp_map = state.get_sftp_connection().await;
            let connections = sftp_map.read().await;
            let client = connections
                .get(&connection_id)
                .ok_or("SFTP connection not found")?;
            client.list_dir(&path).await.map_err(|e| e.to_string())
        }
        "FTP" => {
            let ftp_map = state.get_ftp_connection().await;
            let mut connections = ftp_map.write().await;
            let client = connections
                .get_mut(&connection_id)
                .ok_or("FTP connection not found")?;
            client.list_dir(&path).await.map_err(|e| e.to_string())
        }
        _ => Err(format!("Unsupported protocol: {}", conn_type)),
    }
}

async fn download_remote_file_to_path(
    connection_id: &str,
    remote_path: &str,
    local_path: &str,
    state: &Arc<ConnectionManager>,
) -> Result<FileTransferResponse, String> {
    let conn_type = state.get_connection_type(connection_id).await;

    let result = match conn_type.as_deref() {
        Some("SFTP") => {
            let sftp_map = state.get_sftp_connection().await;
            let connections = sftp_map.read().await;
            let client = connections
                .get(connection_id)
                .ok_or("SFTP connection not found".to_string())?;
            client.download_file(remote_path, local_path).await
        }
        Some("FTP") => {
            let ftp_map = state.get_ftp_connection().await;
            let mut connections = ftp_map.write().await;
            let client = connections
                .get_mut(connection_id)
                .ok_or("FTP connection not found".to_string())?;
            client.download_file(remote_path, local_path).await
        }
        Some(other) => return Err(format!("Unsupported protocol: {}", other)),
        None => {
            // Fallback: try SSH connection (integrated file browser uses SSH connections
            // which are not registered in connection_types)
            let connection = state
                .get_connection(connection_id)
                .await
                .ok_or_else(|| format!("No connection found for '{}'", connection_id))?;
            let client = connection.read().await;
            client.download_file(remote_path, local_path).await
        }
    };

    match result {
        Ok(bytes) => Ok(FileTransferResponse {
            success: true,
            bytes_transferred: Some(bytes),
            data: None,
            error: None,
        }),
        Err(e) => Ok(FileTransferResponse {
            success: false,
            bytes_transferred: None,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

#[tauri::command]
pub async fn download_remote_file(
    connection_id: String,
    remote_path: String,
    local_path: String,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<FileTransferResponse, String> {
    download_remote_file_to_path(&connection_id, &remote_path, &local_path, state.inner()).await
}

#[tauri::command]
pub async fn download_remote_file_confined(
    connection_id: String,
    remote_root: String,
    destination_root: String,
    remote_relative_path: String,
    destination_relative_path: String,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<FileTransferResponse, String> {
    validate_remote_relative_path(&remote_relative_path)?;
    let local_path = resolve_confined_local_path(
        std::path::Path::new(&destination_root),
        &destination_relative_path,
    )?;
    let local_path = local_path
        .to_str()
        .ok_or_else(|| "Local destination path is not valid UTF-8".to_string())?;
    let remote_path = if remote_root == "/" {
        format!("/{}", remote_relative_path)
    } else {
        format!(
            "{}/{}",
            remote_root.trim_end_matches('/'),
            remote_relative_path
        )
    };

    download_remote_file_to_path(&connection_id, &remote_path, local_path, state.inner()).await
}

#[tauri::command]
pub async fn upload_remote_file(
    connection_id: String,
    local_path: String,
    remote_path: String,
    transfer_id: Option<String>,
    app: tauri::AppHandle,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<FileTransferResponse, String> {
    let total_bytes = tokio::fs::metadata(&local_path)
        .await
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let progress_connection_id = connection_id.clone();
    let progress_transfer_id = transfer_id.clone();
    let progress_app = app.clone();
    let mut emit_progress = move |bytes_transferred: u64| {
        emit_file_transfer_progress(
            &progress_app,
            progress_transfer_id.as_deref(),
            &progress_connection_id,
            bytes_transferred,
            total_bytes,
        );
    };

    let conn_type = state.get_connection_type(&connection_id).await;

    let result = match conn_type.as_deref() {
        Some("SFTP") => {
            let sftp_map = state.get_sftp_connection().await;
            let connections = sftp_map.read().await;
            let client = connections
                .get(&connection_id)
                .ok_or("SFTP connection not found".to_string())?;
            client
                .upload_file_with_progress(&local_path, &remote_path, &mut emit_progress)
                .await
        }
        Some("FTP") => {
            let ftp_map = state.get_ftp_connection().await;
            let mut connections = ftp_map.write().await;
            let client = connections
                .get_mut(&connection_id)
                .ok_or("FTP connection not found".to_string())?;
            client.upload_file(&local_path, &remote_path).await
        }
        Some(other) => return Err(format!("Unsupported protocol: {}", other)),
        None => {
            // Fallback: try SSH connection (integrated file browser uses SSH connections
            // which are not registered in connection_types)
            let connection = state
                .get_connection(&connection_id)
                .await
                .ok_or_else(|| format!("No connection found for '{}'", connection_id))?;
            let client = connection.read().await;
            client
                .upload_file_with_progress(&local_path, &remote_path, &mut emit_progress)
                .await
        }
    };

    match result {
        Ok(bytes) => {
            emit_file_transfer_progress(&app, transfer_id.as_deref(), &connection_id, bytes, bytes);
            Ok(FileTransferResponse {
                success: true,
                bytes_transferred: Some(bytes),
                data: None,
                error: None,
            })
        }
        Err(e) => Ok(FileTransferResponse {
            success: false,
            bytes_transferred: None,
            data: None,
            error: {
                tracing::error!(
                    connection_id = %connection_id,
                    transfer_id = ?transfer_id,
                    error = %e,
                    "Remote file upload failed"
                );
                Some(e.to_string())
            },
        }),
    }
}

#[tauri::command]
pub async fn delete_remote_item(
    connection_id: String,
    path: String,
    is_directory: bool,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<CommandResponse, String> {
    let conn_type = state
        .get_connection_type(&connection_id)
        .await
        .ok_or_else(|| format!("No file connection found for '{}'", connection_id))?;

    let result = match conn_type.as_str() {
        "SFTP" => {
            let sftp_map = state.get_sftp_connection().await;
            let connections = sftp_map.read().await;
            let client = connections
                .get(&connection_id)
                .ok_or("SFTP connection not found".to_string())?;
            if is_directory {
                client.delete_dir(&path).await
            } else {
                client.delete_file(&path).await
            }
        }
        "FTP" => {
            let ftp_map = state.get_ftp_connection().await;
            let mut connections = ftp_map.write().await;
            let client = connections
                .get_mut(&connection_id)
                .ok_or("FTP connection not found".to_string())?;
            if is_directory {
                client.delete_dir(&path).await
            } else {
                client.delete_file(&path).await
            }
        }
        _ => return Err(format!("Unsupported protocol: {}", conn_type)),
    };

    match result {
        Ok(_) => Ok(CommandResponse {
            success: true,
            output: Some(format!("Deleted: {}", path)),
            error: None,
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            output: None,
            error: Some(e.to_string()),
        }),
    }
}

#[tauri::command]
pub async fn create_remote_directory(
    connection_id: String,
    path: String,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<CommandResponse, String> {
    let conn_type = state
        .get_connection_type(&connection_id)
        .await
        .ok_or_else(|| format!("No file connection found for '{}'", connection_id))?;

    let result = match conn_type.as_str() {
        "SFTP" => {
            let sftp_map = state.get_sftp_connection().await;
            let connections = sftp_map.read().await;
            let client = connections
                .get(&connection_id)
                .ok_or("SFTP connection not found".to_string())?;
            client.create_dir(&path).await
        }
        "FTP" => {
            let ftp_map = state.get_ftp_connection().await;
            let mut connections = ftp_map.write().await;
            let client = connections
                .get_mut(&connection_id)
                .ok_or("FTP connection not found".to_string())?;
            client.create_dir(&path).await
        }
        _ => return Err(format!("Unsupported protocol: {}", conn_type)),
    };

    match result {
        Ok(_) => Ok(CommandResponse {
            success: true,
            output: Some(format!("Created directory: {}", path)),
            error: None,
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            output: None,
            error: Some(e.to_string()),
        }),
    }
}

#[tauri::command]
pub async fn rename_remote_item(
    connection_id: String,
    old_path: String,
    new_path: String,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<CommandResponse, String> {
    let conn_type = state
        .get_connection_type(&connection_id)
        .await
        .ok_or_else(|| format!("No file connection found for '{}'", connection_id))?;

    let result = match conn_type.as_str() {
        "SFTP" => {
            let sftp_map = state.get_sftp_connection().await;
            let connections = sftp_map.read().await;
            let client = connections
                .get(&connection_id)
                .ok_or("SFTP connection not found".to_string())?;
            client.rename(&old_path, &new_path).await
        }
        "FTP" => {
            let ftp_map = state.get_ftp_connection().await;
            let mut connections = ftp_map.write().await;
            let client = connections
                .get_mut(&connection_id)
                .ok_or("FTP connection not found".to_string())?;
            client.rename(&old_path, &new_path).await
        }
        _ => return Err(format!("Unsupported protocol: {}", conn_type)),
    };

    match result {
        Ok(_) => Ok(CommandResponse {
            success: true,
            output: Some(format!("Renamed '{}' to '{}'", old_path, new_path)),
            error: None,
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            output: None,
            error: Some(e.to_string()),
        }),
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteArchiveSource {
    pub path: String,
    pub is_directory: bool,
}

#[derive(Debug, Clone)]
struct RemoteArchiveEntry {
    remote_path: String,
    relative_path: String,
    is_directory: bool,
}

fn archive_options() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)
}

fn archive_member_name(path: &str) -> Result<String, String> {
    let normalized = path.replace('\\', "/").trim_matches('/').to_string();
    if normalized.is_empty()
        || normalized
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(format!("Invalid archive path: {path}"));
    }
    Ok(normalized)
}

fn add_path_to_zip(
    writer: &mut ZipWriter<File>,
    source: &Path,
    archive_name: &str,
) -> Result<(), String> {
    let archive_name = archive_member_name(archive_name)?;
    let metadata = fs::symlink_metadata(source)
        .map_err(|e| format!("Failed to read '{}': {e}", source.display()))?;

    if metadata.is_dir() {
        writer
            .add_directory(format!("{archive_name}/"), archive_options())
            .map_err(|e| format!("Failed to add directory to archive: {e}"))?;

        let mut children = fs::read_dir(source)
            .map_err(|e| format!("Failed to read '{}': {e}", source.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to enumerate '{}': {e}", source.display()))?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            let child_name = child.file_name().to_string_lossy().into_owned();
            add_path_to_zip(
                writer,
                &child.path(),
                &format!("{archive_name}/{child_name}"),
            )?;
        }
        return Ok(());
    }

    writer
        .start_file(archive_name, archive_options())
        .map_err(|e| format!("Failed to add file to archive: {e}"))?;
    let mut input =
        File::open(source).map_err(|e| format!("Failed to open '{}': {e}", source.display()))?;
    std::io::copy(&mut input, writer)
        .map_err(|e| format!("Failed to write '{}' to archive: {e}", source.display()))?;
    Ok(())
}

fn compress_paths_to_zip(sources: &[(PathBuf, String)], archive_path: &Path) -> Result<(), String> {
    if sources.is_empty() {
        return Err("At least one item is required".to_string());
    }
    if let Some(parent) = archive_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create archive directory: {e}"))?;
    }

    let archive_canonical = archive_path
        .canonicalize()
        .unwrap_or_else(|_| archive_path.to_path_buf());
    for (source, _) in sources {
        let source_canonical = source
            .canonicalize()
            .unwrap_or_else(|_| source.to_path_buf());
        if source_canonical == archive_canonical {
            return Err("The archive cannot contain itself".to_string());
        }
    }
    let output = File::create_new(archive_path)
        .map_err(|e| format!("Failed to create archive '{}': {e}", archive_path.display()))?;
    let mut writer = ZipWriter::new(output);
    for (source, archive_name) in sources {
        if let Err(error) = add_path_to_zip(&mut writer, source, archive_name) {
            drop(writer);
            let _ = fs::remove_file(archive_path);
            return Err(error);
        }
    }
    match writer.finish() {
        Ok(_) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(archive_path);
            Err(format!("Failed to finalize archive: {error}"))
        }
    }
}

fn safe_archive_path(root: &Path, name: &str) -> Result<PathBuf, String> {
    let mut relative = PathBuf::new();
    for component in Path::new(name.replace('\\', "/").as_str()).components() {
        match component {
            std::path::Component::Normal(part) => relative.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::RootDir
            | std::path::Component::Prefix(_)
            | std::path::Component::ParentDir => {
                return Err(format!("Archive entry escapes destination: {name}"));
            }
        }
    }
    if relative.as_os_str().is_empty() {
        return Err(format!("Invalid archive entry: {name}"));
    }
    Ok(root.join(relative))
}

fn extract_zip_to_directory(archive_path: &Path, destination: &Path) -> Result<(), String> {
    let input = File::open(archive_path)
        .map_err(|e| format!("Failed to open archive '{}': {e}", archive_path.display()))?;
    let mut archive = ZipArchive::new(input).map_err(|e| format!("Invalid ZIP archive: {e}"))?;

    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|e| format!("Failed to read archive entry: {e}"))?;
        safe_archive_path(destination, entry.name())?;
    }
    fs::create_dir(destination)
        .map_err(|e| format!("Failed to create extraction directory: {e}"))?;

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|e| format!("Failed to read archive entry: {e}"))?;
        let output_path = safe_archive_path(destination, entry.name())?;
        if entry.is_dir() {
            fs::create_dir_all(&output_path)
                .map_err(|e| format!("Failed to create '{}': {e}", output_path.display()))?;
            continue;
        }
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create '{}': {e}", parent.display()))?;
        }
        let mut output = File::create(&output_path)
            .map_err(|e| format!("Failed to create '{}': {e}", output_path.display()))?;
        std::io::copy(&mut entry, &mut output)
            .map_err(|e| format!("Failed to extract '{}': {e}", entry.name()))?;
    }
    Ok(())
}

fn collect_local_tree(
    root: &Path,
    current: &Path,
    result: &mut Vec<(PathBuf, String, bool)>,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(current)
        .map_err(|e| format!("Failed to read '{}': {e}", current.display()))?;
    let relative = current
        .strip_prefix(root)
        .map_err(|e| format!("Failed to calculate relative path: {e}"))?;
    let relative = relative.to_string_lossy().replace('\\', "/");
    if metadata.is_dir() {
        if !relative.is_empty() {
            result.push((current.to_path_buf(), relative, true));
        }
        let mut children = fs::read_dir(current)
            .map_err(|e| format!("Failed to read '{}': {e}", current.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to enumerate '{}': {e}", current.display()))?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            collect_local_tree(root, &child.path(), result)?;
        }
    } else {
        result.push((current.to_path_buf(), relative, false));
    }
    Ok(())
}

#[tauri::command]
pub async fn compress_local_items(
    paths: Vec<String>,
    archive_path: String,
) -> Result<CommandResponse, String> {
    let sources = paths
        .into_iter()
        .map(|path| {
            let source = PathBuf::from(&path);
            let name = source
                .file_name()
                .ok_or_else(|| format!("Invalid source path: {path}"))?
                .to_string_lossy()
                .into_owned();
            Ok((source, name))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let archive_name = Path::new(&archive_path)
        .file_name()
        .ok_or("Invalid archive filename")?
        .to_string_lossy();
    archive_member_name(&archive_name)?;
    let archive_parent = Path::new(&archive_path)
        .parent()
        .ok_or("Invalid archive directory")?
        .canonicalize()
        .map_err(|e| format!("Invalid archive directory: {e}"))?;
    for (source, _) in &sources {
        let source_parent = source
            .parent()
            .ok_or("Invalid source directory")?
            .canonicalize()
            .map_err(|e| format!("Invalid source directory: {e}"))?;
        if source_parent != archive_parent {
            return Err("All selected items must be in the archive directory".to_string());
        }
    }
    compress_paths_to_zip(&sources, Path::new(&archive_path))?;
    Ok(CommandResponse {
        success: true,
        output: Some(format!("Created archive: {archive_path}")),
        error: None,
    })
}

#[tauri::command]
pub async fn extract_local_archive(
    archive_path: String,
    destination_path: String,
) -> Result<CommandResponse, String> {
    validate_archive_destination(Path::new(&archive_path), Path::new(&destination_path))?;
    extract_zip_to_directory(Path::new(&archive_path), Path::new(&destination_path))?;
    Ok(CommandResponse {
        success: true,
        output: Some(format!("Extracted archive to: {destination_path}")),
        error: None,
    })
}

async fn list_remote_archive_directory(
    connection_id: &str,
    path: &str,
    state: &Arc<ConnectionManager>,
) -> Result<Vec<FileEntry>, String> {
    match state.get_connection_type(connection_id).await.as_deref() {
        Some("SFTP") => {
            let connections = state.get_sftp_connection().await;
            let connections = connections.read().await;
            let client = connections
                .get(connection_id)
                .ok_or("SFTP connection not found")?;
            client.list_dir(path).await.map_err(|e| e.to_string())
        }
        Some("FTP") => {
            let connections = state.get_ftp_connection().await;
            let mut connections = connections.write().await;
            let client = connections
                .get_mut(connection_id)
                .ok_or("FTP connection not found")?;
            client.list_dir(path).await.map_err(|e| e.to_string())
        }
        Some(other) => Err(format!("Unsupported protocol: {other}")),
        None => {
            let connection = state
                .get_connection(connection_id)
                .await
                .ok_or_else(|| format!("No connection found for '{connection_id}'"))?;
            let client = connection.read().await;
            let os_info = get_os_info(connection_id, &client, state).await;
            let output = client
                .execute_command(&os_info.list_files_cmd(path))
                .await
                .map_err(|e| e.to_string())?;
            Ok(output
                .lines()
                .filter_map(crate::ls_parser::parse_ls_long_line)
                .collect())
        }
    }
}

fn remote_join(base: &str, name: &str) -> String {
    if base == "/" {
        format!("/{name}")
    } else {
        format!("{}/{}", base.trim_end_matches('/'), name)
    }
}

fn remote_basename(path: &str) -> Result<String, String> {
    path.trim_end_matches('/')
        .rsplit('/')
        .find(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("Invalid remote path: {path}"))
}

fn remote_parent(path: &str) -> Result<&str, String> {
    path.rsplit_once('/')
        .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
        .ok_or_else(|| format!("Invalid remote path: {path}"))
}

fn validate_archive_destination(archive_path: &Path, destination: &Path) -> Result<(), String> {
    let name = destination
        .file_name()
        .ok_or("Invalid extraction directory")?
        .to_string_lossy();
    archive_member_name(&name)?;
    if destination.exists() {
        return Err(format!(
            "Extraction directory already exists: {}",
            destination.display()
        ));
    }
    let archive_parent = archive_path
        .parent()
        .ok_or("Invalid archive directory")?
        .canonicalize()
        .map_err(|e| format!("Invalid archive directory: {e}"))?;
    let destination_parent = destination
        .parent()
        .ok_or("Invalid extraction directory")?
        .canonicalize()
        .map_err(|e| format!("Invalid extraction directory: {e}"))?;
    if archive_parent != destination_parent {
        return Err("Extraction directory must be beside the archive".to_string());
    }
    Ok(())
}

async fn collect_remote_archive_entries(
    connection_id: &str,
    remote_path: &str,
    relative_path: &str,
    is_directory: bool,
    state: &Arc<ConnectionManager>,
    result: &mut Vec<RemoteArchiveEntry>,
) -> Result<(), String> {
    let mut pending = vec![(
        remote_path.to_string(),
        relative_path.to_string(),
        is_directory,
    )];
    while let Some((current_remote_path, current_relative_path, current_is_directory)) =
        pending.pop()
    {
        result.push(RemoteArchiveEntry {
            remote_path: current_remote_path.clone(),
            relative_path: current_relative_path.clone(),
            is_directory: current_is_directory,
        });
        if !current_is_directory {
            continue;
        }
        let mut children =
            list_remote_archive_directory(connection_id, &current_remote_path, state).await?;
        children.reverse();
        for entry in children {
            if entry.name == "." || entry.name == ".." {
                continue;
            }
            pending.push((
                remote_join(&current_remote_path, &entry.name),
                format!("{current_relative_path}/{}", entry.name),
                matches!(entry.file_type, FileEntryType::Directory),
            ));
        }
    }
    Ok(())
}

async fn download_remote_archive_file(
    connection_id: &str,
    remote_path: &str,
    local_path: &Path,
    state: &Arc<ConnectionManager>,
) -> Result<(), String> {
    let response = download_remote_file_to_path(
        connection_id,
        remote_path,
        local_path
            .to_str()
            .ok_or_else(|| "Temporary path is not valid UTF-8")?,
        state,
    )
    .await?;
    if response.success {
        Ok(())
    } else {
        Err(response
            .error
            .unwrap_or_else(|| format!("Failed to download '{remote_path}'")))
    }
}

async fn upload_remote_archive_file(
    connection_id: &str,
    local_path: &Path,
    remote_path: &str,
    state: &Arc<ConnectionManager>,
) -> Result<(), String> {
    let local_path = local_path
        .to_str()
        .ok_or_else(|| "Temporary path is not valid UTF-8")?;
    let result = match state.get_connection_type(connection_id).await.as_deref() {
        Some("SFTP") => {
            let connections = state.get_sftp_connection().await;
            let connections = connections.read().await;
            let client = connections
                .get(connection_id)
                .ok_or("SFTP connection not found")?;
            client.upload_file(local_path, remote_path).await
        }
        Some("FTP") => {
            let connections = state.get_ftp_connection().await;
            let mut connections = connections.write().await;
            let client = connections
                .get_mut(connection_id)
                .ok_or("FTP connection not found")?;
            client.upload_file(local_path, remote_path).await
        }
        Some(other) => return Err(format!("Unsupported protocol: {other}")),
        None => {
            let connection = state
                .get_connection(connection_id)
                .await
                .ok_or_else(|| format!("No connection found for '{connection_id}'"))?;
            let client = connection.read().await;
            client.upload_file(local_path, remote_path).await
        }
    };
    result
        .map(|_| ())
        .map_err(|e| format!("Failed to upload '{remote_path}': {e}"))
}

async fn create_remote_directory_for_archive(
    connection_id: &str,
    path: &str,
    state: &Arc<ConnectionManager>,
) -> Result<(), String> {
    match state.get_connection_type(connection_id).await.as_deref() {
        Some("SFTP") => {
            let connections = state.get_sftp_connection().await;
            let connections = connections.read().await;
            let client = connections
                .get(connection_id)
                .ok_or("SFTP connection not found")?;
            client.create_dir(path).await.map_err(|e| e.to_string())
        }
        Some("FTP") => {
            let connections = state.get_ftp_connection().await;
            let mut connections = connections.write().await;
            let client = connections
                .get_mut(connection_id)
                .ok_or("FTP connection not found")?;
            client.create_dir(path).await.map_err(|e| e.to_string())
        }
        Some(other) => Err(format!("Unsupported protocol: {other}")),
        None => {
            let connection = state
                .get_connection(connection_id)
                .await
                .ok_or_else(|| format!("No connection found for '{connection_id}'"))?;
            let client = connection.read().await;
            let command = format!("mkdir -p '{}'", shell_escape_single_quoted(path));
            client
                .execute_command(&command)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
    }
}

const REMOTE_ZIP_CREATE_SCRIPT: &str = r#"
import os
import sys
import zipfile

parent, archive_name, *names = sys.argv[1:]
os.chdir(parent)
created = False
try:
    archive = zipfile.ZipFile(archive_name, 'x', compression=zipfile.ZIP_DEFLATED)
    created = True
    with archive:
        for name in names:
            if os.path.islink(name):
                raise ValueError('Symbolic links are not supported: ' + name)
            if os.path.isdir(name):
                for root, dirs, files in os.walk(name, followlinks=False):
                    if any(os.path.islink(os.path.join(root, item)) for item in dirs + files):
                        raise ValueError('Symbolic links are not supported: ' + root)
                    archive.write(root, root + '/')
                    for file_name in files:
                        path = os.path.join(root, file_name)
                        archive.write(path, path)
            else:
                archive.write(name, name)
except:
    if created:
        try:
            os.unlink(archive_name)
        except OSError:
            pass
    raise
"#;

const REMOTE_ZIP_EXTRACT_SCRIPT: &str = r#"
import os
import pathlib
import shutil
import stat
import sys
import zipfile

parent, archive_name, destination_name = sys.argv[1:]
os.chdir(parent)
with zipfile.ZipFile(archive_name) as archive:
    for entry in archive.infolist():
        path = pathlib.PurePosixPath(entry.filename)
        mode = entry.external_attr >> 16
        if (path.is_absolute() or '\\' in entry.filename or
                any(part in ('', '.', '..') for part in entry.filename.rstrip('/').split('/')) or
                stat.S_ISLNK(mode)):
            raise ValueError('Invalid ZIP entry: ' + entry.filename)
    os.mkdir(destination_name)
    try:
        archive.extractall(destination_name)
    except:
        shutil.rmtree(destination_name)
        raise
"#;

fn remote_python_command(script: &str, arguments: &[&str]) -> String {
    let mut command = format!("python3 -c '{}'", shell_escape_single_quoted(script));
    for argument in arguments {
        command.push_str(&format!(" '{}'", shell_escape_single_quoted(argument)));
    }
    command
}

#[tauri::command]
pub async fn compress_remote_items(
    connection_id: String,
    items: Vec<RemoteArchiveSource>,
    archive_path: String,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<CommandResponse, String> {
    if items.is_empty() {
        return Err("At least one item is required".to_string());
    }
    let archive_name = remote_basename(&archive_path)?;
    archive_member_name(&archive_name)?;
    let parent = remote_parent(&archive_path)?;
    for item in &items {
        if remote_parent(&item.path)? != parent {
            return Err("All selected items must be in the archive directory".to_string());
        }
        archive_member_name(&remote_basename(&item.path)?)?;
    }
    let existing = list_remote_archive_directory(&connection_id, parent, state.inner()).await?;
    if existing.iter().any(|entry| entry.name == archive_name) {
        return Err(format!("Archive already exists: {archive_path}"));
    }
    if state.get_connection_type(&connection_id).await.is_none() {
        let connection = state
            .get_connection(&connection_id)
            .await
            .ok_or_else(|| format!("No connection found for '{connection_id}'"))?;
        let names = items
            .iter()
            .map(|item| remote_basename(&item.path))
            .collect::<Result<Vec<_>, _>>()?;
        let mut arguments = vec![parent, archive_name.as_str()];
        arguments.extend(names.iter().map(String::as_str));
        let command = remote_python_command(REMOTE_ZIP_CREATE_SCRIPT, &arguments);
        connection
            .read()
            .await
            .execute_command_checked(&command)
            .await
            .map_err(|error| error.to_string())?;
        return Ok(CommandResponse {
            success: true,
            output: Some(format!("Created archive: {archive_path}")),
            error: None,
        });
    }
    let work_dir =
        tempfile::tempdir().map_err(|e| format!("Failed to create temporary workspace: {e}"))?;
    let result = async {
        let staging = work_dir.path().join("staging");
        fs::create_dir_all(&staging)
            .map_err(|e| format!("Failed to create staging directory: {e}"))?;
        let mut entries = Vec::new();
        let mut roots = Vec::new();
        for item in items {
            let name = remote_basename(&item.path)?;
            let relative = archive_member_name(&name)?;
            roots.push((staging.join(&relative), relative.clone()));
            collect_remote_archive_entries(
                &connection_id,
                &item.path,
                &relative,
                item.is_directory,
                state.inner(),
                &mut entries,
            )
            .await?;
        }
        for entry in entries {
            let local_path = staging.join(&entry.relative_path);
            if entry.is_directory {
                fs::create_dir_all(&local_path)
                    .map_err(|e| format!("Failed to create staging directory: {e}"))?;
            } else {
                if let Some(parent) = local_path.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|e| format!("Failed to create staging directory: {e}"))?;
                }
                download_remote_archive_file(
                    &connection_id,
                    &entry.remote_path,
                    &local_path,
                    state.inner(),
                )
                .await?;
            }
        }
        let local_archive = work_dir.path().join("archive.zip");
        compress_paths_to_zip(&roots, &local_archive)?;
        upload_remote_archive_file(&connection_id, &local_archive, &archive_path, state.inner())
            .await?;
        Ok::<(), String>(())
    }
    .await;
    result?;
    Ok(CommandResponse {
        success: true,
        output: Some(format!("Created archive: {archive_path}")),
        error: None,
    })
}

#[tauri::command]
pub async fn extract_remote_archive(
    connection_id: String,
    archive_path: String,
    destination_path: String,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<CommandResponse, String> {
    let destination_name = remote_basename(&destination_path)?;
    archive_member_name(&destination_name)?;
    if remote_parent(&archive_path)? != remote_parent(&destination_path)? {
        return Err("Extraction directory must be beside the archive".to_string());
    }
    let parent = remote_parent(&destination_path)?;
    let existing = list_remote_archive_directory(&connection_id, parent, state.inner()).await?;
    if existing.iter().any(|entry| entry.name == destination_name) {
        return Err(format!(
            "Extraction directory already exists: {destination_path}"
        ));
    }
    if state.get_connection_type(&connection_id).await.is_none() {
        let connection = state
            .get_connection(&connection_id)
            .await
            .ok_or_else(|| format!("No connection found for '{connection_id}'"))?;
        let archive_name = remote_basename(&archive_path)?;
        let command = remote_python_command(
            REMOTE_ZIP_EXTRACT_SCRIPT,
            &[parent, &archive_name, &destination_name],
        );
        connection
            .read()
            .await
            .execute_command_checked(&command)
            .await
            .map_err(|error| error.to_string())?;
        return Ok(CommandResponse {
            success: true,
            output: Some(format!("Extracted archive to: {destination_path}")),
            error: None,
        });
    }
    let work_dir =
        tempfile::tempdir().map_err(|e| format!("Failed to create temporary workspace: {e}"))?;
    let result = async {
        let local_archive = work_dir.path().join("archive.zip");
        download_remote_archive_file(&connection_id, &archive_path, &local_archive, state.inner())
            .await?;
        let extracted = work_dir.path().join("extracted");
        extract_zip_to_directory(&local_archive, &extracted)?;

        create_remote_directory_for_archive(&connection_id, &destination_path, state.inner())
            .await?;
        let mut entries = Vec::new();
        collect_local_tree(&extracted, &extracted, &mut entries)?;
        entries.sort_by_key(|(_, path, is_directory)| {
            (!*is_directory, path.matches('/').count(), path.clone())
        });
        for (local_path, relative_path, is_directory) in entries {
            let remote_path = remote_join(&destination_path, &relative_path);
            if is_directory {
                create_remote_directory_for_archive(&connection_id, &remote_path, state.inner())
                    .await?;
            } else {
                upload_remote_archive_file(
                    &connection_id,
                    &local_path,
                    &remote_path,
                    state.inner(),
                )
                .await?;
            }
        }
        Ok::<(), String>(())
    }
    .await;
    result?;
    Ok(CommandResponse {
        success: true,
        output: Some(format!("Extracted archive to: {destination_path}")),
        error: None,
    })
}

// ========== Local Filesystem Commands ==========

#[tauri::command]
pub async fn list_local_files(path: String) -> Result<Vec<FileEntry>, String> {
    use std::fs;

    let dir_path = std::path::Path::new(&path);
    if !dir_path.exists() {
        return Err(format!("Path does not exist: {}", path));
    }
    if !dir_path.is_dir() {
        return Err(format!("Path is not a directory: {}", path));
    }

    let read_dir = fs::read_dir(dir_path)
        .map_err(|e| format!("Failed to read directory '{}': {}", path, e))?;

    let mut entries: Vec<FileEntry> = Vec::new();
    for item in read_dir {
        let item = match item {
            Ok(i) => i,
            Err(_) => continue,
        };

        let name = item.file_name().to_string_lossy().to_string();
        // Skip hidden files starting with . (optional, but common in FTP clients)
        // Actually, let's show all files like FileZilla does

        let metadata = match item.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        let file_type = if metadata.is_dir() {
            FileEntryType::Directory
        } else if metadata.file_type().is_symlink() {
            FileEntryType::Symlink
        } else {
            FileEntryType::File
        };

        let size = metadata.len();

        let modified = metadata.modified().ok().map(|t| {
            let duration = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
            let secs = duration.as_secs() as i64;
            format_unix_timestamp(secs)
        });

        #[cfg(unix)]
        let permissions = {
            use std::os::unix::fs::PermissionsExt;
            let mode = metadata.permissions().mode();
            Some(format_unix_permissions(mode))
        };
        #[cfg(not(unix))]
        let permissions: Option<String> = None;

        // Numeric uid/gid — the filesystem exposes ids, not names, without a
        // passwd/group lookup. Symbolic names come from remote `ls -l` listings.
        #[cfg(unix)]
        let (owner, group): (Option<String>, Option<String>) = {
            use std::os::unix::fs::MetadataExt;
            (
                Some(metadata.uid().to_string()),
                Some(metadata.gid().to_string()),
            )
        };
        #[cfg(not(unix))]
        let (owner, group): (Option<String>, Option<String>) = (None, None);

        entries.push(FileEntry {
            name,
            size,
            modified,
            permissions,
            file_type,
            owner,
            group,
        });
    }

    // Sort: directories first, then files, alphabetical within each group
    entries.sort_by(|a, b| {
        let a_is_dir = matches!(a.file_type, FileEntryType::Directory);
        let b_is_dir = matches!(b.file_type, FileEntryType::Directory);
        match (a_is_dir, b_is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        }
    });

    Ok(entries)
}

/// Format a unix timestamp (seconds since epoch) into an ISO-like datetime string.
fn format_unix_timestamp(secs: i64) -> String {
    // Simple manual conversion for local display
    // This avoids pulling in chrono — we just need a readable date string
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Compute year, month, day from days since epoch (1970-01-01)
    let mut y = 1970i64;
    let mut remaining_days = days;

    loop {
        let days_in_year = if is_leap_year(y) { 366 } else { 365 };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        y += 1;
    }

    let month_days = if is_leap_year(y) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut m = 0usize;
    for (i, &md) in month_days.iter().enumerate() {
        if remaining_days < md as i64 {
            m = i;
            break;
        }
        remaining_days -= md as i64;
    }

    let d = remaining_days + 1;
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        y,
        m + 1,
        d,
        hours,
        minutes,
        seconds
    )
}

fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

/// Format Unix file mode bits into a human-readable rwx string.
#[cfg(unix)]
fn format_unix_permissions(mode: u32) -> String {
    let mut s = String::with_capacity(10);
    // File type
    s.push(match mode & 0o170000 {
        0o040000 => 'd',
        0o120000 => 'l',
        _ => '-',
    });
    // Owner
    s.push(if mode & 0o400 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o200 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o100 != 0 { 'x' } else { '-' });
    // Group
    s.push(if mode & 0o040 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o020 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o010 != 0 { 'x' } else { '-' });
    // Other
    s.push(if mode & 0o004 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o002 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o001 != 0 { 'x' } else { '-' });
    s
}

#[tauri::command]
pub async fn get_home_directory() -> Result<String, String> {
    dirs::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .ok_or_else(|| "Could not determine home directory".to_string())
}

#[tauri::command]
pub async fn delete_local_item(path: String, is_directory: bool) -> Result<(), String> {
    use std::fs;
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(format!("Path does not exist: {}", path));
    }
    if is_directory {
        fs::remove_dir_all(p).map_err(|e| format!("Failed to delete directory '{}': {}", path, e))
    } else {
        fs::remove_file(p).map_err(|e| format!("Failed to delete file '{}': {}", path, e))
    }
}

#[tauri::command]
pub async fn rename_local_item(old_path: String, new_path: String) -> Result<(), String> {
    use std::fs;
    let p = std::path::Path::new(&old_path);
    if !p.exists() {
        return Err(format!("Path does not exist: {}", old_path));
    }
    fs::rename(&old_path, &new_path)
        .map_err(|e| format!("Failed to rename '{}' to '{}': {}", old_path, new_path, e))
}

fn validate_remote_relative_path(remote_relative_path: &str) -> Result<(), String> {
    if remote_relative_path.is_empty()
        || remote_relative_path.starts_with('/')
        || remote_relative_path.starts_with('\\')
        || remote_relative_path.contains('\\')
    {
        return Err(format!(
            "Unsafe remote relative path: {}",
            remote_relative_path
        ));
    }

    for (index, component) in remote_relative_path.split('/').enumerate() {
        let has_windows_prefix = index == 0
            && component
                .as_bytes()
                .get(1)
                .is_some_and(|separator| *separator == b':');
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.contains('\0')
            || has_windows_prefix
        {
            return Err(format!(
                "Unsafe remote relative path: {}",
                remote_relative_path
            ));
        }
    }
    Ok(())
}

fn resolve_confined_local_path(
    destination_root: &std::path::Path,
    remote_relative_path: &str,
) -> Result<std::path::PathBuf, String> {
    if !destination_root.is_absolute() {
        return Err("Destination root must be absolute".to_string());
    }
    validate_remote_relative_path(remote_relative_path)?;

    let mut resolved = destination_root.to_path_buf();
    for component in remote_relative_path.split('/') {
        resolved.push(component);
    }

    if !resolved.starts_with(destination_root) {
        return Err(format!(
            "Remote path escapes destination root: {}",
            remote_relative_path
        ));
    }
    Ok(resolved)
}

#[tauri::command]
pub async fn create_local_directory(path: String) -> Result<(), String> {
    use std::fs;
    fs::create_dir_all(&path).map_err(|e| format!("Failed to create directory '{}': {}", path, e))
}

#[tauri::command]
pub async fn create_local_directory_confined(
    destination_root: String,
    relative_path: String,
) -> Result<(), String> {
    let path =
        resolve_confined_local_path(std::path::Path::new(&destination_root), &relative_path)?;
    std::fs::create_dir_all(&path)
        .map_err(|e| format!("Failed to create directory '{}': {}", path.display(), e))
}

#[tauri::command]
pub async fn open_in_os(path: String) -> Result<(), String> {
    open::that(&path).map_err(|e| format!("Failed to open '{}': {}", path, e))
}

/// Metadata for a local path. Returned by `stat_local_path` so the frontend can
/// cheaply decide (without recursing) whether a dropped filesystem entry is a
/// file or a directory before building an upload plan.
///
/// `is_symlink` is true iff the path itself is a symlink (we read the link's own
/// metadata). `is_directory` / `size` follow the link's target; if the target is
/// missing (broken link or network share down) we fall back to the link's own
/// metadata so `exists` stays true and the path is treated as a file (size 0).
#[derive(Debug, Clone, serde::Serialize)]
pub struct LocalPathStat {
    pub exists: bool,
    pub is_directory: bool,
    pub is_symlink: bool,
    pub size: u64,
}

#[tauri::command]
pub async fn stat_local_path(path: String) -> Result<LocalPathStat, String> {
    use std::fs;
    let p = std::path::Path::new(&path);
    // `symlink_metadata` never follows the link — works on Windows without the
    // SE_CREATE_SYMBOLIC_LINK privilege.
    let sym = match fs::symlink_metadata(p) {
        Ok(m) => m,
        Err(_) => {
            return Ok(LocalPathStat {
                exists: false,
                is_directory: false,
                is_symlink: false,
                size: 0,
            })
        }
    };
    let is_symlink = sym.file_type().is_symlink();
    // Follow the link; if the target is missing (broken link, unmounted share,
    // etc.) fall back to the link's own metadata so `exists` stays true and we
    // still surface the entry as a (zero-byte) file to the upload pipeline.
    let md = match fs::metadata(p) {
        Ok(m) => m,
        Err(_) => sym,
    };
    Ok(LocalPathStat {
        exists: true,
        is_directory: md.is_dir(),
        is_symlink,
        size: if md.is_file() { md.len() } else { 0 },
    })
}

// ========== Directory Synchronization ==========

/// A file entry with a relative path (used for recursive listing comparisons).
#[derive(Debug, Clone, Serialize)]
pub struct SyncFileEntry {
    pub relative_path: String,
    pub name: String,
    pub size: u64,
    pub modified: Option<String>,
    pub file_type: FileEntryType,
}

/// Recursively list all files/dirs under a local directory, returning relative paths.
#[tauri::command]
pub async fn list_local_files_recursive(
    path: String,
    exclude_patterns: Vec<String>,
) -> Result<Vec<SyncFileEntry>, String> {
    use std::fs;

    fn relative_path_to_string(path: &std::path::Path) -> String {
        path.components()
            .map(|component| component.as_os_str().to_string_lossy())
            .filter(|component| !component.is_empty())
            .collect::<Vec<_>>()
            .join("/")
    }

    fn walk_dir(
        base: &std::path::Path,
        current: &std::path::Path,
        exclude: &[String],
        results: &mut Vec<SyncFileEntry>,
    ) -> Result<(), String> {
        let read_dir = fs::read_dir(current)
            .map_err(|e| format!("Failed to read '{}': {}", current.display(), e))?;

        for item in read_dir {
            let item = match item {
                Ok(i) => i,
                Err(_) => continue,
            };
            let name = item.file_name().to_string_lossy().to_string();

            // Check exclude patterns
            if matches_exclude(&name, exclude) {
                continue;
            }

            let metadata = match item.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };

            let rel_path = item
                .path()
                .strip_prefix(base)
                .unwrap_or(item.path().as_path())
                .to_path_buf();
            let rel_path = relative_path_to_string(&rel_path);

            let file_type = if metadata.is_dir() {
                FileEntryType::Directory
            } else if metadata.file_type().is_symlink() {
                FileEntryType::Symlink
            } else {
                FileEntryType::File
            };

            let modified = metadata.modified().ok().map(|t| {
                let duration = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
                let secs = duration.as_secs() as i64;
                format_unix_timestamp(secs)
            });

            results.push(SyncFileEntry {
                relative_path: rel_path.clone(),
                name: name.clone(),
                size: metadata.len(),
                modified,
                file_type: file_type.clone(),
            });

            // Recurse into directories
            if metadata.is_dir() {
                walk_dir(base, &item.path(), exclude, results)?;
            }
        }
        Ok(())
    }

    let base_path = std::path::Path::new(&path);
    if !base_path.exists() || !base_path.is_dir() {
        return Err(format!(
            "Path does not exist or is not a directory: {}",
            path
        ));
    }

    let mut results = Vec::new();
    walk_dir(base_path, base_path, &exclude_patterns, &mut results)?;

    // Sort: directories first, then by relative path
    results.sort_by(|a, b| {
        let a_dir = matches!(a.file_type, FileEntryType::Directory);
        let b_dir = matches!(b.file_type, FileEntryType::Directory);
        match (a_dir, b_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.relative_path.cmp(&b.relative_path),
        }
    });

    Ok(results)
}

fn walk_sftp<'a>(
    sftp: &'a russh_sftp::client::SftpSession,
    base: &'a str,
    current: &'a str,
    exclude: &'a [String],
    results: &'a mut Vec<SyncFileEntry>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
    Box::pin(async move {
        let entries = crate::sftp_client::list_sftp_dir(sftp, current)
            .await
            .map_err(|e| e.to_string())?;
        for entry in entries {
            if matches_exclude(&entry.name, exclude) {
                continue;
            }
            let full_path = if current == "/" {
                format!("/{}", entry.name)
            } else {
                format!("{}/{}", current, entry.name)
            };
            let relative_path = full_path
                .strip_prefix(base)
                .unwrap_or(&full_path)
                .trim_start_matches('/')
                .to_string();
            let is_dir = matches!(entry.file_type, FileEntryType::Directory);

            results.push(SyncFileEntry {
                relative_path,
                name: entry.name,
                size: entry.size,
                modified: entry.modified,
                file_type: entry.file_type,
            });

            if is_dir {
                walk_sftp(sftp, base, &full_path, exclude, results).await?;
            }
        }
        Ok(())
    })
}

/// Recursively list all files/dirs under a remote directory (SSH/SFTP/FTP).
#[tauri::command]
pub async fn list_remote_files_recursive(
    connection_id: String,
    path: String,
    exclude_patterns: Vec<String>,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<Vec<SyncFileEntry>, String> {
    let conn_type = state.get_connection_type(&connection_id).await;

    let mut results = Vec::new();

    match conn_type.as_deref() {
        Some("SFTP") => {
            let sftp_map = state.get_sftp_connection().await;
            let connections = sftp_map.read().await;
            let client = connections
                .get(&connection_id)
                .ok_or("SFTP connection not found")?;
            let sftp = client.sftp_session().map_err(|e| e.to_string())?;
            walk_sftp(sftp, &path, &path, &exclude_patterns, &mut results).await?;
        }
        Some("FTP") => {
            let ftp_map = state.get_ftp_connection().await;
            let mut connections = ftp_map.write().await;
            let client = connections
                .get_mut(&connection_id)
                .ok_or("FTP connection not found")?;

            // FTP recursive walk — iterative with a queue since we need &mut
            let mut dirs_to_visit: Vec<String> = vec![path.clone()];
            while let Some(dir) = dirs_to_visit.pop() {
                let entries = client.list_dir(&dir).await.map_err(|e| e.to_string())?;
                for entry in entries {
                    if matches_exclude(&entry.name, &exclude_patterns) {
                        continue;
                    }
                    let full_path = if dir == "/" {
                        format!("/{}", entry.name)
                    } else {
                        format!("{}/{}", dir, entry.name)
                    };
                    let rel = full_path
                        .strip_prefix(&path)
                        .unwrap_or(&full_path)
                        .trim_start_matches('/')
                        .to_string();

                    let is_dir = matches!(entry.file_type, FileEntryType::Directory);

                    results.push(SyncFileEntry {
                        relative_path: rel.clone(),
                        name: entry.name.clone(),
                        size: entry.size,
                        modified: entry.modified.clone(),
                        file_type: entry.file_type.clone(),
                    });

                    if is_dir {
                        dirs_to_visit.push(full_path);
                    }
                }
            }
        }
        None | Some("SSH") => {
            let connection = state
                .get_connection(&connection_id)
                .await
                .ok_or("SSH connection not found")?;
            let client = connection.read().await;
            let sftp = client
                .open_sftp_session()
                .await
                .map_err(|e| e.to_string())?;
            walk_sftp(&sftp, &path, &path, &exclude_patterns, &mut results).await?;
        }
        Some(other) => return Err(format!("Unsupported protocol: {}", other)),
    }

    // Sort similarly
    results.sort_by(|a, b| {
        let a_dir = matches!(a.file_type, FileEntryType::Directory);
        let b_dir = matches!(b.file_type, FileEntryType::Directory);
        match (a_dir, b_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.relative_path.cmp(&b.relative_path),
        }
    });

    Ok(results)
}

/// Simple glob-like pattern matching for exclude filter.
fn matches_exclude(name: &str, patterns: &[String]) -> bool {
    for pat in patterns {
        if pat.starts_with("*.") {
            // Extension match
            let ext = &pat[1..]; // e.g., ".log"
            if name.ends_with(ext) {
                return true;
            }
        } else if name == pat {
            return true;
        }
    }
    false
}

// ========== Desktop (RDP/VNC) Commands ==========

/// Connect to a remote desktop via RDP or VNC
#[tauri::command]
pub async fn desktop_connect(
    connection_id: String,
    request: crate::desktop_protocol::DesktopConnectRequest,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<crate::desktop_protocol::DesktopConnectResponse, String> {
    tracing::info!(
        "Desktop connect: {} ({}) to {}:{}",
        connection_id,
        request.protocol,
        request.host,
        request.port
    );

    let (width, height) = state
        .create_desktop_connection(connection_id, &request)
        .await
        .map_err(|e| e.to_string())?;

    Ok(crate::desktop_protocol::DesktopConnectResponse { width, height })
}

/// Disconnect a remote desktop session
#[tauri::command]
pub async fn desktop_disconnect(
    connection_id: String,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<(), String> {
    tracing::info!("Desktop disconnect: {}", connection_id);
    state
        .close_desktop_connection(&connection_id)
        .await
        .map_err(|e| e.to_string())
}

/// Send a keyboard event to a remote desktop session
#[tauri::command]
pub async fn desktop_send_key(
    connection_id: String,
    key_code: u32,
    down: bool,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<(), String> {
    let client = state
        .get_desktop_connection(&connection_id)
        .await
        .ok_or_else(|| format!("Desktop connection not found: {}", connection_id))?;
    let c = client.read().await;
    c.send_key(key_code, down).await.map_err(|e| e.to_string())
}

/// Send a mouse/pointer event to a remote desktop session
#[tauri::command]
pub async fn desktop_send_pointer(
    connection_id: String,
    x: u16,
    y: u16,
    button_mask: u8,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<(), String> {
    let client = state
        .get_desktop_connection(&connection_id)
        .await
        .ok_or_else(|| format!("Desktop connection not found: {}", connection_id))?;
    let c = client.read().await;
    c.send_pointer(x, y, button_mask)
        .await
        .map_err(|e| e.to_string())
}

/// Request a full framebuffer update from a remote desktop session
#[tauri::command]
pub async fn desktop_request_frame(
    connection_id: String,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<(), String> {
    let client = state
        .get_desktop_connection(&connection_id)
        .await
        .ok_or_else(|| format!("Desktop connection not found: {}", connection_id))?;
    let c = client.read().await;
    c.request_full_frame().await.map_err(|e| e.to_string())
}

/// Send clipboard text to a remote desktop session
#[tauri::command]
pub async fn desktop_set_clipboard(
    connection_id: String,
    text: String,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<(), String> {
    let client = state
        .get_desktop_connection(&connection_id)
        .await
        .ok_or_else(|| format!("Desktop connection not found: {}", connection_id))?;
    let c = client.read().await;
    c.set_clipboard(text).await.map_err(|e| e.to_string())
}

/// Request a remote desktop session to resize to the given dimensions.
/// For RDP: sends a display resize request to the remote server.
/// For VNC: no-op (client-side scaling is used instead).
#[tauri::command]
pub async fn desktop_resize(
    connection_id: String,
    width: u16,
    height: u16,
    state: State<'_, Arc<ConnectionManager>>,
) -> Result<(), String> {
    let client = state
        .get_desktop_connection(&connection_id)
        .await
        .ok_or_else(|| format!("Desktop connection not found: {}", connection_id))?;
    let mut c = client.write().await;
    c.resize(width, height).await.map_err(|e| e.to_string())
}

// ========== Native Menu i18n ==========

/// Rebuild the native macOS menu bar with translated labels from the frontend.
/// On non-macOS platforms this is a no-op.
#[tauri::command]
pub async fn update_menu_language(
    app: tauri::AppHandle,
    translations: std::collections::HashMap<String, String>,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let menu = crate::build_app_menu(&app, move |key: &str| {
            translations
                .get(key)
                .cloned()
                .unwrap_or_else(|| crate::default_menu_text(key))
        })
        .map_err(|e| e.to_string())?;
        app.set_menu(menu).map_err(|e| e.to_string())?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, translations);
    }
    Ok(())
}

/// Return the OS-level locale string (e.g. "en-US", "zh-CN").
/// Used on first run to match the app language to the user's system preference.
#[tauri::command]
pub fn get_system_locale() -> Result<String, String> {
    sys_locale::get_locale().ok_or_else(|| "Failed to detect system locale".to_string())
}

#[cfg(test)]
mod archive_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn compress_and_extract_nested_files_and_empty_directories() {
        let workspace = TempDir::new().unwrap();
        let folder = workspace.path().join("资料");
        fs::create_dir(&folder).unwrap();
        fs::create_dir(folder.join("empty")).unwrap();
        fs::write(folder.join("文件.txt"), "hello ZIP").unwrap();
        let archive = workspace.path().join("资料.zip");

        compress_paths_to_zip(&[(folder, "资料".to_string())], &archive).unwrap();
        let destination = workspace.path().join("extracted");
        extract_zip_to_directory(&archive, &destination).unwrap();

        assert_eq!(
            fs::read_to_string(destination.join("资料/文件.txt")).unwrap(),
            "hello ZIP"
        );
        assert!(destination.join("资料/empty").is_dir());
        assert!(compress_paths_to_zip(&[], &workspace.path().join("empty.zip")).is_err());
        assert!(extract_zip_to_directory(&archive, &destination).is_err());
    }

    #[test]
    fn extraction_rejects_traversal_and_existing_destination() {
        let workspace = TempDir::new().unwrap();
        let destination = workspace.path().join("extracted");
        assert!(safe_archive_path(&destination, "../outside").is_err());
        assert!(safe_archive_path(&destination, "folder/../../outside").is_err());
        assert!(safe_archive_path(&destination, "/outside").is_err());
        assert!(safe_archive_path(&destination, "folder\\..\\outside").is_err());
        assert_eq!(
            safe_archive_path(&destination, "folder/文件.txt").unwrap(),
            destination.join("folder/文件.txt")
        );
    }
}

// ========== Local Filesystem Tests ==========

#[cfg(test)]
mod local_fs_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_dir() -> TempDir {
        let dir = TempDir::new().unwrap();
        // Create some test files and directories
        fs::write(dir.path().join("file1.txt"), "hello").unwrap();
        fs::write(dir.path().join("file2.rs"), "fn main() {}").unwrap();
        fs::create_dir(dir.path().join("subdir")).unwrap();
        fs::write(dir.path().join("subdir").join("nested.txt"), "nested").unwrap();
        dir
    }

    #[tokio::test]
    async fn test_list_local_files() {
        let dir = create_test_dir();
        let path = dir.path().to_string_lossy().to_string();
        let result = list_local_files(path).await;
        assert!(result.is_ok());
        let entries = result.unwrap();
        // subdir should come first (directories first)
        assert_eq!(entries[0].name, "subdir");
        assert!(matches!(entries[0].file_type, FileEntryType::Directory));
        // Then files alphabetically
        let file_names: Vec<&str> = entries[1..].iter().map(|e| e.name.as_str()).collect();
        assert!(file_names.contains(&"file1.txt"));
        assert!(file_names.contains(&"file2.rs"));
    }

    #[tokio::test]
    async fn test_list_local_files_nonexistent() {
        let result = list_local_files("/nonexistent/path/xyz".to_string()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not exist"));
    }

    #[tokio::test]
    async fn test_get_home_directory() {
        let result = get_home_directory().await;
        assert!(result.is_ok());
        let home = result.unwrap();
        assert!(!home.is_empty());
        assert!(std::path::Path::new(&home).exists());
    }

    #[tokio::test]
    async fn test_delete_local_file() {
        let dir = create_test_dir();
        let file_path = dir.path().join("file1.txt").to_string_lossy().to_string();
        assert!(std::path::Path::new(&file_path).exists());
        let result = delete_local_item(file_path.clone(), false).await;
        assert!(result.is_ok());
        assert!(!std::path::Path::new(&file_path).exists());
    }

    #[tokio::test]
    async fn test_delete_local_directory() {
        let dir = create_test_dir();
        let sub_path = dir.path().join("subdir").to_string_lossy().to_string();
        assert!(std::path::Path::new(&sub_path).exists());
        let result = delete_local_item(sub_path.clone(), true).await;
        assert!(result.is_ok());
        assert!(!std::path::Path::new(&sub_path).exists());
    }

    #[tokio::test]
    async fn test_rename_local_item() {
        let dir = create_test_dir();
        let old_path = dir.path().join("file1.txt").to_string_lossy().to_string();
        let new_path = dir.path().join("renamed.txt").to_string_lossy().to_string();
        let result = rename_local_item(old_path.clone(), new_path.clone()).await;
        assert!(result.is_ok());
        assert!(!std::path::Path::new(&old_path).exists());
        assert!(std::path::Path::new(&new_path).exists());
    }

    #[tokio::test]
    async fn test_create_local_directory() {
        let dir = create_test_dir();
        let new_dir = dir.path().join("new_subdir").to_string_lossy().to_string();
        let result = create_local_directory(new_dir.clone()).await;
        assert!(result.is_ok());
        assert!(std::path::Path::new(&new_dir).is_dir());
    }

    #[tokio::test]
    async fn confined_directory_creation_stays_under_destination_root() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_string_lossy().to_string();

        create_local_directory_confined(root, "nested/子目录".to_string())
            .await
            .unwrap();

        assert!(dir.path().join("nested").join("子目录").is_dir());
    }

    #[tokio::test]
    async fn confined_directory_creation_rejects_windows_traversal() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("destination");
        fs::create_dir(&root).unwrap();

        let result = create_local_directory_confined(
            root.to_string_lossy().to_string(),
            "nested\\..\\outside".to_string(),
        )
        .await;

        assert!(result.is_err());
        assert!(!dir.path().join("outside").exists());
    }

    #[test]
    fn confined_local_path_accepts_portable_nested_paths() {
        let dir = TempDir::new().unwrap();

        let resolved = resolve_confined_local_path(dir.path(), "子目录/report 1.txt").unwrap();

        assert_eq!(resolved, dir.path().join("子目录").join("report 1.txt"));
    }

    #[test]
    fn confined_local_path_rejects_escaping_or_ambiguous_paths() {
        let dir = TempDir::new().unwrap();
        let unsafe_paths = [
            "",
            ".",
            "../escape.txt",
            "nested/../../escape.txt",
            "/absolute.txt",
            "\\absolute.txt",
            "nested//file.txt",
            "nested/./file.txt",
            "C:/escape.txt",
            "C:\\escape.txt",
            "\\\\server\\share\\escape.txt",
            "nested\\..\\escape.txt",
        ];

        for relative_path in unsafe_paths {
            assert!(
                resolve_confined_local_path(dir.path(), relative_path).is_err(),
                "unsafe path should be rejected: {relative_path:?}"
            );
        }
    }

    #[test]
    fn confined_local_path_allows_dots_within_normal_names() {
        let dir = TempDir::new().unwrap();

        for relative_path in ["report..txt", "..hidden", "nested/v1..2.txt"] {
            assert!(
                resolve_confined_local_path(dir.path(), relative_path).is_ok(),
                "normal name should be accepted: {relative_path:?}"
            );
        }
    }

    #[test]
    fn confined_local_path_requires_an_absolute_root() {
        assert!(resolve_confined_local_path(
            std::path::Path::new("relative-root"),
            "nested/file.txt"
        )
        .is_err());
    }

    #[tokio::test]
    async fn test_list_local_files_recursive_returns_portable_relative_paths() {
        let dir = create_test_dir();
        let path = dir.path().to_string_lossy().to_string();
        let entries = list_local_files_recursive(path, vec![]).await.unwrap();
        let relative_paths: Vec<&str> = entries
            .iter()
            .map(|entry| entry.relative_path.as_str())
            .collect();

        assert!(relative_paths.contains(&"subdir/nested.txt"));
        assert!(
            relative_paths
                .iter()
                .all(|relative_path| !relative_path.contains('\\')),
            "relative paths should use forward slashes: {:?}",
            relative_paths
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_format_unix_permissions() {
        assert_eq!(format_unix_permissions(0o100644), "-rw-r--r--");
        assert_eq!(format_unix_permissions(0o040755), "drwxr-xr-x");
        assert_eq!(format_unix_permissions(0o100755), "-rwxr-xr-x");
        assert_eq!(format_unix_permissions(0o120777), "lrwxrwxrwx");
    }

    #[test]
    fn test_format_unix_timestamp() {
        // 2024-01-01 00:00:00 UTC = 1704067200
        let s = format_unix_timestamp(1704067200);
        assert_eq!(s, "2024-01-01 00:00:00");
    }
}

#[cfg(test)]
mod proxy_config_tests {
    use super::*;

    fn request(proxy_type: Option<&str>) -> ConnectRequest {
        ConnectRequest {
            connection_id: "c1".to_string(),
            host: "example.com".to_string(),
            port: 22,
            username: "root".to_string(),
            auth_method: "password".to_string(),
            password: Some("pw".to_string()),
            key_path: None,
            passphrase: None,
            compression: None,
            keepalive_enabled: None,
            keepalive_interval: None,
            keepalive_max: None,
            proxy_type: proxy_type.map(|s| s.to_string()),
            proxy_host: None,
            proxy_port: None,
            proxy_username: None,
            proxy_password: None,
        }
    }

    #[test]
    fn no_proxy_when_type_none_or_none_type() {
        assert!(build_proxy(&request(None)).unwrap().is_none());
        assert!(build_proxy(&request(Some("none"))).unwrap().is_none());
        assert!(build_proxy(&request(Some(""))).unwrap().is_none());
    }

    /// Request with a proxy host set, so type/host validation reaches the type.
    fn request_with_host(proxy_type: &str) -> ConnectRequest {
        let mut req = request(Some(proxy_type));
        req.proxy_host = Some("proxy.local".to_string());
        req
    }

    #[test]
    fn maps_supported_proxy_types() {
        let http = build_proxy(&request_with_host("http")).unwrap().unwrap();
        assert_eq!(http.proxy_type, ProxyType::Http);
        let socks5 = build_proxy(&request_with_host("socks5")).unwrap().unwrap();
        assert_eq!(socks5.proxy_type, ProxyType::Socks5);
        let socks4 = build_proxy(&request_with_host("socks4")).unwrap().unwrap();
        assert_eq!(socks4.proxy_type, ProxyType::Socks4);
    }

    #[test]
    fn requires_proxy_host() {
        let err = build_proxy(&request(Some("http"))).unwrap_err();
        assert!(err.contains("Proxy host is required"));
    }

    #[test]
    fn rejects_unknown_proxy_type() {
        let err = build_proxy(&request_with_host("ftp")).unwrap_err();
        assert!(err.contains("Invalid proxy type"));
    }

    #[test]
    fn carries_proxy_credentials_and_port() {
        let mut req = request(Some("socks5"));
        req.proxy_host = Some("proxy.local".to_string());
        req.proxy_port = Some(1080);
        req.proxy_username = Some("user".to_string());
        req.proxy_password = Some("pass".to_string());
        let cfg = build_proxy(&req).unwrap().unwrap();
        assert_eq!(cfg.host, "proxy.local");
        assert_eq!(cfg.port, 1080);
        assert_eq!(cfg.username.as_deref(), Some("user"));
        assert_eq!(cfg.password.as_deref(), Some("pass"));
    }

    #[test]
    fn defaults_proxy_port_to_8080() {
        let mut req = request(Some("http"));
        req.proxy_host = Some("proxy.local".to_string());
        let cfg = build_proxy(&req).unwrap().unwrap();
        assert_eq!(cfg.port, 8080);
    }
}
