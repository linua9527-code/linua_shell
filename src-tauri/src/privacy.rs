use serde::Serialize;
use std::fs;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

const PRIVACY_FILE_NAME: &str = "privacy-unlock.conf";

/// @brief 返回入口口令状态，不向前端暴露口令内容。
#[derive(Debug, Serialize)]
pub struct PrivacyStatus {
    pub configured: bool,
}

/// @brief 返回跨平台的固定配置文件路径。
///
/// Windows 使用 `%APPDATA%`，Linux 使用 `$XDG_CONFIG_HOME` 或 `~/.config`，
/// 具体目录由 Tauri 根据应用标识自动解析，避免手工拼接平台分隔符。
fn privacy_file_path(app: &AppHandle) -> Result<PathBuf, String> {
    let directory = app
        .path()
        .app_config_dir()
        .map_err(|error| format!("Failed to resolve privacy configuration directory: {error}"))?;
    Ok(directory.join(PRIVACY_FILE_NAME))
}

/// @brief 读取固定位置的入口口令配置。
///
/// 配置文件使用 UTF-8 纯文本，文件末尾允许存在 CR/LF；口令正文中的空格会原样保留。
fn read_phrase(path: &std::path::Path) -> Result<Option<Vec<u8>>, String> {
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(path)
        .map_err(|error| format!("Failed to read privacy configuration: {error}"))?;
    let content = content.strip_prefix('\u{feff}').unwrap_or(&content);
    let phrase = content.trim_end_matches(&['\r', '\n'][..]);
    if phrase.is_empty() {
        return Err("Privacy configuration is empty".to_string());
    }

    Ok(Some(phrase.as_bytes().to_vec()))
}

/// @brief 使用常量时间比较入口口令，降低长度和内容差异带来的时序信息泄露。
fn phrases_match(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let length = left.len().max(right.len());

    for index in 0..length {
        let left_byte = left.get(index).copied().unwrap_or(0);
        let right_byte = right.get(index).copied().unwrap_or(0);
        difference |= usize::from(left_byte ^ right_byte);
    }

    difference == 0
}

/// @brief 返回固定配置文件中是否存在有效入口口令。
#[tauri::command]
pub fn privacy_get_status(app: AppHandle) -> Result<PrivacyStatus, String> {
    let path = privacy_file_path(&app)?;
    Ok(PrivacyStatus {
        configured: read_phrase(&path)?.is_some(),
    })
}

/// @brief 校验入口口令，入口口令只从固定配置文件读取。
#[tauri::command]
pub fn privacy_verify_phrase(app: AppHandle, phrase: String) -> Result<bool, String> {
    let path = privacy_file_path(&app)?;
    let Some(stored_phrase) = read_phrase(&path)? else {
        return Ok(false);
    };

    Ok(phrases_match(stored_phrase.as_slice(), phrase.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::{phrases_match, read_phrase};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn phrase_comparison_requires_exact_bytes() {
        assert!(phrases_match(b"open sesame", b"open sesame"));
        assert!(!phrases_match(b"open sesame", b"open  sesame"));
        assert!(!phrases_match(b"open sesame", b"open sesame "));

        let long_phrase = vec![0; 257];
        let short_phrase = vec![0; 1];
        assert!(!phrases_match(&long_phrase, &short_phrase));
    }

    #[test]
    fn config_phrase_preserves_spaces_and_removes_line_ending() {
        let directory = tempdir().expect("temporary directory should be created");
        let path = directory.path().join("privacy-unlock.conf");
        fs::write(&path, "  open sesame  \r\n").expect("configuration should be written");

        assert_eq!(
            read_phrase(&path).unwrap().as_deref(),
            Some(b"  open sesame  ".as_slice())
        );
    }
}
