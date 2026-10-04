use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::{Mutex, OnceCell};

/// Detected OS family of a remote host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OsFamily {
    /// Debian, Ubuntu, Linux Mint, Pop!_OS, etc.
    Debian,
    /// RHEL, CentOS, Fedora, Rocky, AlmaLinux, Amazon Linux, Oracle Linux
    RedHat,
    /// Alpine Linux (musl-based, BusyBox coreutils)
    Alpine,
    /// openSUSE, SLES
    Suse,
    /// Arch Linux, Manjaro
    Arch,
    /// Generic Linux — has /proc but we couldn't identify the family
    GenericLinux,
    /// macOS / Darwin
    MacOS,
    /// FreeBSD / OpenBSD / NetBSD
    Bsd,
    /// Completely unknown
    Unknown,
}

/// Cached information about a remote host's OS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OsInfo {
    pub family: OsFamily,
    /// Raw ID from /etc/os-release (e.g. "ubuntu", "centos", "alpine")
    pub id: String,
    /// Human-readable name (e.g. "Ubuntu 22.04 LTS")
    pub pretty_name: String,
    /// Whether GNU coreutils are available (vs BusyBox)
    pub has_gnu_coreutils: bool,
}

impl Default for OsInfo {
    fn default() -> Self {
        Self {
            family: OsFamily::Unknown,
            id: String::new(),
            pretty_name: String::new(),
            has_gnu_coreutils: true,
        }
    }
}

/// Per-connection OS info cache.
///
/// Uses one `OnceCell` per connection so that concurrent callers share a
/// single in-flight detection rather than each spawning their own.
pub struct OsInfoCache {
    cells: Arc<Mutex<HashMap<String, Arc<OnceCell<OsInfo>>>>>,
}

impl OsInfoCache {
    pub fn new() -> Self {
        Self {
            cells: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Return the cached OS info, running `init` exactly once per connection.
    /// Concurrent callers for the same `connection_id` block until the first
    /// detection completes, then all receive the same cached result.
    pub async fn get_or_init<F, Fut>(&self, connection_id: &str, init: F) -> OsInfo
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = OsInfo>,
    {
        let cell = {
            let mut cells = self.cells.lock().await;
            cells
                .entry(connection_id.to_string())
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };
        cell.get_or_init(init).await.clone()
    }

    /// Remove cached info when a connection is closed.
    pub async fn remove(&self, connection_id: &str) {
        self.cells.lock().await.remove(connection_id);
    }
}

/// Detect the remote OS by running a lightweight probe command over SSH.
///
/// The detection runs a single compound command that reads /etc/os-release,
/// falls back to uname, and probes for tool availability — all in one
/// round-trip to minimise latency.
pub async fn detect_os(client: &crate::ssh::SshClient) -> OsInfo {
    // Single compound command — works on virtually every POSIX system.
    // We collect:
    //   1. ID and PRETTY_NAME from /etc/os-release
    //   2. uname -s as fallback kernel name
    //   3. Probe for GNU coreutils
    let probe = r#"
(
  # 1. /etc/os-release (present on all modern distros)
  if [ -f /etc/os-release ]; then
    . /etc/os-release
    echo "ID=${ID:-unknown}"
    echo "PRETTY_NAME=${PRETTY_NAME:-unknown}"
    echo "ID_LIKE=${ID_LIKE:-}"
  else
    echo "ID=unknown"
    echo "PRETTY_NAME=unknown"
    echo "ID_LIKE="
  fi

  # 2. Kernel name
  echo "UNAME=$(uname -s 2>/dev/null || echo unknown)"

  # GNU ls supports --version; BusyBox does not
  if ls --version 2>&1 | head -1 | grep -qi 'GNU\|coreutils'; then
    echo "HAS_GNU_COREUTILS=1"
  else
    echo "HAS_GNU_COREUTILS=0"
  fi
)
"#;

    let output = match client.execute_command(probe).await {
        Ok(o) => o,
        Err(_) => return OsInfo::default(),
    };

    let mut id = String::new();
    let mut pretty_name = String::new();
    let mut id_like = String::new();
    let mut uname = String::new();
    let mut has_gnu_coreutils = true;

    for line in output.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("ID=") {
            id = val.trim_matches('"').to_lowercase();
        } else if let Some(val) = line.strip_prefix("PRETTY_NAME=") {
            pretty_name = val.trim_matches('"').to_string();
        } else if let Some(val) = line.strip_prefix("ID_LIKE=") {
            id_like = val.trim_matches('"').to_lowercase();
        } else if let Some(val) = line.strip_prefix("UNAME=") {
            uname = val.to_lowercase();
        } else if let Some(val) = line.strip_prefix("HAS_GNU_COREUTILS=") {
            has_gnu_coreutils = val == "1";
        }
    }

    let family = classify_family(&id, &id_like, &uname);

    OsInfo {
        family,
        id,
        pretty_name,
        has_gnu_coreutils,
    }
}

/// Classify the OS family from the ID, ID_LIKE, and uname fields.
fn classify_family(id: &str, id_like: &str, uname: &str) -> OsFamily {
    // Check ID first (exact match)
    match id {
        "debian" | "ubuntu" | "linuxmint" | "pop" | "elementary" | "zorin" | "kali"
        | "raspbian" | "deepin" | "kylin" => return OsFamily::Debian,

        "rhel" | "centos" | "fedora" | "rocky" | "almalinux" | "ol" | "amzn" | "scientific"
        | "eurolinux" | "anolis" | "openeuler" | "tencentos" | "alinux" => return OsFamily::RedHat,

        "alpine" => return OsFamily::Alpine,

        "opensuse-leap" | "opensuse-tumbleweed" | "sles" | "suse" => return OsFamily::Suse,

        "arch" | "manjaro" | "endeavouros" | "garuda" => return OsFamily::Arch,

        _ => {}
    }

    // Check ID_LIKE for derivative distros
    for token in id_like.split_whitespace() {
        match token {
            "debian" | "ubuntu" => return OsFamily::Debian,
            "rhel" | "fedora" | "centos" => return OsFamily::RedHat,
            "suse" | "opensuse" => return OsFamily::Suse,
            "arch" => return OsFamily::Arch,
            _ => {}
        }
    }

    // Fallback to uname
    match uname.as_ref() {
        "darwin" => OsFamily::MacOS,
        "freebsd" | "openbsd" | "netbsd" => OsFamily::Bsd,
        "linux" => OsFamily::GenericLinux,
        _ => OsFamily::Unknown,
    }
}

// ─── Distro-aware command builders ───────────────────────────────────────────

impl OsInfo {
    /// List files command.
    /// GNU ls supports `--time-style=long-iso`; BusyBox and macOS do not.
    pub fn list_files_cmd(&self, path: &str) -> String {
        fn shell_quote(value: &str) -> String {
            format!("'{}'", value.replace('\'', "'\"'\"'"))
        }

        let quoted_path = shell_quote(path);
        if self.has_gnu_coreutils {
            format!("ls -la --time-style=long-iso {}", quoted_path)
        } else {
            // BusyBox / macOS ls — no --time-style, but -la still works
            format!("ls -la {}", quoted_path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_debian() {
        assert_eq!(classify_family("ubuntu", "", "linux"), OsFamily::Debian);
        assert_eq!(classify_family("debian", "", "linux"), OsFamily::Debian);
        assert_eq!(
            classify_family("linuxmint", "ubuntu debian", "linux"),
            OsFamily::Debian
        );
    }

    #[test]
    fn test_classify_redhat() {
        assert_eq!(classify_family("centos", "", "linux"), OsFamily::RedHat);
        assert_eq!(classify_family("rhel", "", "linux"), OsFamily::RedHat);
        assert_eq!(classify_family("fedora", "", "linux"), OsFamily::RedHat);
        assert_eq!(classify_family("rocky", "", "linux"), OsFamily::RedHat);
        assert_eq!(classify_family("almalinux", "", "linux"), OsFamily::RedHat);
        assert_eq!(classify_family("amzn", "", "linux"), OsFamily::RedHat);
        assert_eq!(classify_family("ol", "", "linux"), OsFamily::RedHat);
    }

    #[test]
    fn test_classify_alpine() {
        assert_eq!(classify_family("alpine", "", "linux"), OsFamily::Alpine);
    }

    #[test]
    fn test_classify_suse() {
        assert_eq!(
            classify_family("opensuse-leap", "", "linux"),
            OsFamily::Suse
        );
        assert_eq!(classify_family("sles", "", "linux"), OsFamily::Suse);
    }

    #[test]
    fn test_classify_arch() {
        assert_eq!(classify_family("arch", "", "linux"), OsFamily::Arch);
        assert_eq!(classify_family("manjaro", "", "linux"), OsFamily::Arch);
    }

    #[test]
    fn test_classify_by_id_like() {
        assert_eq!(
            classify_family("pop", "ubuntu debian", "linux"),
            OsFamily::Debian
        );
        assert_eq!(
            classify_family("eurolinux", "rhel fedora centos", "linux"),
            OsFamily::RedHat
        );
    }

    #[test]
    fn test_classify_macos() {
        assert_eq!(classify_family("unknown", "", "darwin"), OsFamily::MacOS);
    }

    #[test]
    fn test_classify_unknown_linux() {
        assert_eq!(
            classify_family("unknown", "", "linux"),
            OsFamily::GenericLinux
        );
    }

    #[test]
    fn test_list_files_gnu() {
        let info = OsInfo {
            has_gnu_coreutils: true,
            ..Default::default()
        };
        assert!(info.list_files_cmd("/tmp").contains("--time-style"));
    }

    #[test]
    fn test_list_files_busybox() {
        let info = OsInfo {
            has_gnu_coreutils: false,
            ..Default::default()
        };
        assert!(!info.list_files_cmd("/tmp").contains("--time-style"));
    }

    #[test]
    fn test_list_files_quotes_apostrophes() {
        let info = OsInfo {
            has_gnu_coreutils: true,
            ..Default::default()
        };

        assert_eq!(
            info.list_files_cmd("/tmp/dir's folder"),
            "ls -la --time-style=long-iso '/tmp/dir'\"'\"'s folder'"
        );
    }
}
