use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::{engine::general_purpose::STANDARD, Engine};
use keyring::Entry;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Manager, State};
use zeroize::Zeroizing;

const BLOCK_SIZE: usize = 4096;
const MAX_SIZE: usize = 32 * 1024 * 1024;

#[derive(Default)]
pub struct SecureStore(Mutex<Option<Store>>);

struct Store {
    path: PathBuf,
    cipher: Aes256Gcm,
}

/// @brief 为认证加密载荷添加固定块填充，文件中不保存明文标识或配置字段。
fn seal(cipher: &Aes256Gcm, data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() > MAX_SIZE {
        return Err("Storage size limit exceeded".into());
    }
    let padded_size = (data.len() + 8).div_ceil(BLOCK_SIZE) * BLOCK_SIZE;
    let mut padded = Zeroizing::new(vec![0u8; padded_size]);
    padded[..8].copy_from_slice(&(data.len() as u64).to_le_bytes());
    padded[8..8 + data.len()].copy_from_slice(data);
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, padded.as_slice())
        .map_err(|_| "Failed to encrypt storage".to_string())?;
    let mut result = nonce.to_vec();
    result.extend_from_slice(&ciphertext);
    Ok(result)
}

/// @brief 校验认证标签后解密；损坏的数据不会被当作空配置覆盖。
fn unseal(cipher: &Aes256Gcm, data: &[u8]) -> Result<Zeroizing<Vec<u8>>, String> {
    if data.len() < 12 + 16 + BLOCK_SIZE || data.len() > MAX_SIZE + BLOCK_SIZE + 28 {
        return Err("Invalid encrypted storage size".into());
    }
    let padded = Zeroizing::new(
        cipher
            .decrypt(Nonce::from_slice(&data[..12]), &data[12..])
            .map_err(|_| "Storage authentication failed".to_string())?,
    );
    let length = u64::from_le_bytes(
        padded[..8]
            .try_into()
            .map_err(|_| "Invalid storage payload".to_string())?,
    );
    let length = usize::try_from(length).map_err(|_| "Invalid storage length".to_string())?;
    if length > MAX_SIZE || length > padded.len() - 8 {
        return Err("Invalid storage length".into());
    }
    Ok(Zeroizing::new(padded[8..8 + length].to_vec()))
}

/// @brief 密钥只保存在操作系统凭据库，不写入配置目录或前端存储。
fn open_store(app: &AppHandle) -> Result<Store, String> {
    let directory = app
        .path()
        .app_local_data_dir()
        .map_err(|error| error.to_string())?
        .join("runtime-cache");
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    let path = directory.join("index-v2.bin");
    let entry = Entry::new(&app.config().identifier, "runtime-index-v2")
        .map_err(|error| format!("Credential storage unavailable: {error}"))?;
    let encoded_key = Zeroizing::new(match entry.get_password() {
        Ok(value) => value,
        Err(keyring::Error::NoEntry) if !path.exists() => {
            let generated = Aes256Gcm::generate_key(&mut OsRng);
            let encoded = STANDARD.encode(generated);
            entry
                .set_password(&encoded)
                .map_err(|error| format!("Failed to protect storage key: {error}"))?;
            encoded
        }
        Err(error) => return Err(format!("Failed to load storage key: {error}")),
    });
    let key = Zeroizing::new(
        STANDARD
            .decode(encoded_key.as_bytes())
            .map_err(|_| "Invalid storage key".to_string())?,
    );
    let cipher = Aes256Gcm::new_from_slice(key.as_slice())
        .map_err(|_| "Invalid storage key length".to_string())?;
    Ok(Store { path, cipher })
}

/// @brief 在相同目录原子替换已加密数据，避免中断写入破坏原文件。
fn write_atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    let directory = path.parent().ok_or("Invalid storage directory")?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(directory).map_err(|error| error.to_string())?;
    temporary
        .write_all(data)
        .map_err(|error| error.to_string())?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temporary.persist(path).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    fs::File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn secure_store_load(
    app: AppHandle,
    state: State<'_, SecureStore>,
) -> Result<HashMap<String, String>, String> {
    let mut state = state.0.lock().map_err(|_| "Storage lock poisoned")?;
    if state.is_none() {
        *state = Some(open_store(&app)?);
    }
    let store = state.as_ref().ok_or("Storage not initialized")?;
    if !store.path.exists() {
        return Ok(HashMap::new());
    }
    if fs::metadata(&store.path)
        .map_err(|error| error.to_string())?
        .len()
        > (MAX_SIZE + BLOCK_SIZE + 28) as u64
    {
        return Err("Storage size limit exceeded".into());
    }
    let encrypted = fs::read(&store.path).map_err(|error| error.to_string())?;
    let plaintext = unseal(&store.cipher, &encrypted)?;
    serde_json::from_slice(plaintext.as_slice()).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn secure_store_save(
    records: HashMap<String, String>,
    state: State<'_, SecureStore>,
) -> Result<(), String> {
    let state = state.0.lock().map_err(|_| "Storage lock poisoned")?;
    let store = state.as_ref().ok_or("Storage not initialized")?;
    let plaintext =
        Zeroizing::new(serde_json::to_vec(&records).map_err(|error| error.to_string())?);
    write_atomic(&store.path, &seal(&store.cipher, plaintext.as_slice())?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_records_round_trip_without_plaintext() {
        let cipher = Aes256Gcm::new(&Aes256Gcm::generate_key(&mut OsRng));
        let data = br#"{"host":"private.example.com","password":"secret"}"#;
        let encrypted = seal(&cipher, data).unwrap();
        assert_eq!(unseal(&cipher, &encrypted).unwrap().as_slice(), data);
        assert!(!encrypted.windows(6).any(|bytes| bytes == b"secret"));
        assert_eq!(encrypted.len(), BLOCK_SIZE + 28);
        assert_ne!(encrypted, seal(&cipher, data).unwrap());
    }

    #[test]
    fn corrupted_or_wrong_key_records_are_rejected() {
        let cipher = Aes256Gcm::new(&Aes256Gcm::generate_key(&mut OsRng));
        let other = Aes256Gcm::new(&Aes256Gcm::generate_key(&mut OsRng));
        let mut encrypted = seal(&cipher, b"secret").unwrap();
        assert!(unseal(&other, &encrypted).is_err());
        encrypted[20] ^= 1;
        assert!(unseal(&cipher, &encrypted).is_err());
        assert!(unseal(&cipher, b"short").is_err());
    }

    #[test]
    fn atomic_write_replaces_existing_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("index-v2.bin");
        write_atomic(&path, b"first").unwrap();
        write_atomic(&path, b"second").unwrap();
        assert_eq!(fs::read(path).unwrap(), b"second");
    }
}
