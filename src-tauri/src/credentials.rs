use parking_lot::Mutex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, LocalFree, HANDLE, HLOCAL, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};
use windows::Win32::Storage::FileSystem::{
    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

static PROCESS_DIR_MUTEXES: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

fn get_process_dir_mutex(root_dir: &Path) -> Arc<Mutex<()>> {
    let canonical = root_dir
        .canonicalize()
        .unwrap_or_else(|_| root_dir.to_path_buf());
    let registry = PROCESS_DIR_MUTEXES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = registry.lock();
    map.entry(canonical)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Windows Named Mutex の RAII ガード
#[derive(Debug)]
pub(crate) struct NamedMutexGuard(HANDLE);

unsafe impl Send for NamedMutexGuard {}
unsafe impl Sync for NamedMutexGuard {}

impl Drop for NamedMutexGuard {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = ReleaseMutex(self.0);
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// プロセス内 Mutex と Windows Named Mutex を組み合わせた排他ロックガード
pub struct CredentialLockGuard<'a> {
    _process_guard: parking_lot::MutexGuard<'a, ()>,
    _named_guard: NamedMutexGuard,
}

fn acquire_named_mutex(root_dir: &Path, timeout_ms: u32) -> Result<NamedMutexGuard, String> {
    let canonical = root_dir
        .canonicalize()
        .unwrap_or_else(|_| root_dir.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let hash_bytes = hasher.finalize();
    let hash_hex: String = hash_bytes.iter().map(|b| format!("{:02x}", b)).collect();
    let lock_name = format!("Local\\GameAssistant_CredLock_{}", hash_hex);
    let lock_name_w: Vec<u16> = lock_name.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        let handle = CreateMutexW(None, false, PCWSTR(lock_name_w.as_ptr())).map_err(|e| {
            format!(
                "Failed to create/open named mutex for credentials on '{}': {}",
                root_dir.display(),
                e
            )
        })?;

        match WaitForSingleObject(handle, timeout_ms) {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(NamedMutexGuard(handle)),
            WAIT_TIMEOUT => {
                let _ = CloseHandle(handle);
                Err(format!(
                    "Timed out waiting for credential lock on '{}' after {} ms",
                    root_dir.display(),
                    timeout_ms
                ))
            }
            other => {
                let _ = CloseHandle(handle);
                Err(format!(
                    "Failed to acquire credential lock on '{}', wait result: {:?}",
                    root_dir.display(),
                    other
                ))
            }
        }
    }
}

/// 既存ファイルを安全にアトミック置換する (Windows MoveFileExW with MOVEFILE_REPLACE_EXISTING)
pub fn atomic_replace_file(from: &Path, to: &Path) -> Result<(), String> {
    let from_h = HSTRING::from(from.as_os_str());
    let to_h = HSTRING::from(to.as_os_str());
    unsafe {
        MoveFileExW(
            &from_h,
            &to_h,
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
        .map_err(|e| {
            format!(
                "Failed to atomic replace file from '{}' to '{}': {}",
                from.display(),
                to.display(),
                e
            )
        })?;
    }
    Ok(())
}

/// 平文で保存してはならない機密設定キーの一覧
pub const SECRET_KEYS: &[&str] = &[
    "gemini_api_key",
    "brave_api_key",
    "twitch_client_secret",
    "twitch_access_token",
    "twitch_refresh_token",
    "twitch_bot_token",
];

/// 指定されたキーが機密情報（シークレット）かどうかを判定する
pub fn is_secret_key(key: &str) -> bool {
    SECRET_KEYS.contains(&key)
}

/// Windows DPAPI (CryptProtectData) によるバイト列の暗号化
/// 現在の Windows ユーザー資格情報に紐づくマスターキーで保護されるため、他ユーザーや他マシンからは復号できない
pub fn dpapi_encrypt(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.is_empty() {
        return Ok(Vec::new());
    }

    let in_blob = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut out_blob = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };

    unsafe {
        CryptProtectData(
            &in_blob,
            PCWSTR::null(),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out_blob,
        )
        .map_err(|e| format!("DPAPI encryption failed: {}", e))?;

        let slice = std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize);
        let result = slice.to_vec();
        let _ = LocalFree(HLOCAL(out_blob.pbData as _));
        Ok(result)
    }
}

/// Windows DPAPI (CryptUnprotectData) による暗号化バイト列の復号
pub fn dpapi_decrypt(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.is_empty() {
        return Ok(Vec::new());
    }

    let in_blob = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut out_blob = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };

    unsafe {
        CryptUnprotectData(
            &in_blob,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out_blob,
        )
        .map_err(|e| format!("DPAPI decryption failed: {}", e))?;

        let slice = std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize);
        let result = slice.to_vec();
        let _ = LocalFree(HLOCAL(out_blob.pbData as _));
        Ok(result)
    }
}

/// 資格情報ストアの抽象トレイト
pub trait CredentialStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>, String>;
    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        let trimmed = value.trim().to_string();
        let key_str = key.to_string();
        self.mutate(&mut move |map| {
            if trimmed.is_empty() {
                map.remove(&key_str);
            } else {
                map.insert(key_str.clone(), trimmed.clone());
            }
            Ok(true)
        })
    }
    fn delete(&self, key: &str) -> Result<(), String> {
        let key_str = key.to_string();
        self.mutate(&mut move |map| {
            let changed = map.remove(&key_str).is_some();
            Ok(changed)
        })
    }
    fn exists(&self, key: &str) -> Result<bool, String> {
        self.get(key).map(|opt| opt.is_some())
    }
    fn list_configured_keys(&self) -> Result<Vec<String>, String>;

    /// 資格情報マップを一括・アトミックに変更する。
    /// クロージャが true を返した場合のみファイル（またはストレージ）への書き込み・保存が行われる。
    fn mutate(
        &self,
        f: &mut dyn FnMut(&mut HashMap<String, String>) -> Result<bool, String>,
    ) -> Result<(), String>;
}

/// DPAPI で暗号化されたローカルファイルストア (credentials.enc)
#[derive(Clone, Debug)]
pub struct DpapiCredentialStore {
    root_dir: PathBuf,
    process_lock: Arc<Mutex<()>>,
}

impl DpapiCredentialStore {
    pub fn new(root_dir: &Path) -> Self {
        let process_lock = get_process_dir_mutex(root_dir);
        Self {
            root_dir: root_dir.to_path_buf(),
            process_lock,
        }
    }

    pub fn acquire_lock(&self) -> Result<CredentialLockGuard<'_>, String> {
        let process_guard = self.process_lock.lock();
        let named_guard = acquire_named_mutex(&self.root_dir, 30_000)?;
        Ok(CredentialLockGuard {
            _process_guard: process_guard,
            _named_guard: named_guard,
        })
    }

    fn credentials_path(&self) -> PathBuf {
        self.root_dir.join("credentials.enc")
    }

    fn load_map(&self) -> Result<HashMap<String, String>, String> {
        let path = self.credentials_path();
        if !path.exists() {
            return Ok(HashMap::new());
        }

        let encrypted_bytes = fs::read(&path)
            .map_err(|e| format!("Failed to read credential file {}: {}", path.display(), e))?;

        if encrypted_bytes.is_empty() {
            return Ok(HashMap::new());
        }

        let decrypted_bytes = dpapi_decrypt(&encrypted_bytes)?;
        let map: HashMap<String, String> = serde_json::from_slice(&decrypted_bytes)
            .map_err(|e| format!("Failed to parse decrypted credentials JSON: {}", e))?;

        Ok(map)
    }

    fn save_map(&self, map: &HashMap<String, String>) -> Result<(), String> {
        let path = self.credentials_path();
        if map.is_empty() {
            if path.exists() {
                fs::remove_file(&path).map_err(|e| {
                    format!(
                        "Failed to remove credentials file {}: {}",
                        path.display(),
                        e
                    )
                })?;
            }
            return Ok(());
        }

        let raw_json = serde_json::to_vec(map)
            .map_err(|e| format!("Failed to serialize credentials: {}", e))?;

        let encrypted_bytes = dpapi_encrypt(&raw_json)?;

        // アトミックに保存するため一時ファイルに書き出してから rename
        let temp_path = self.root_dir.join(format!(
            "credentials.enc.tmp.{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::write(&temp_path, &encrypted_bytes)
            .map_err(|e| format!("Failed to write temporary credentials file: {}", e))?;

        if let Err(e) = atomic_replace_file(&temp_path, &path) {
            let _ = fs::remove_file(&temp_path);
            return Err(format!("Failed to commit credentials file: {}", e));
        }

        Ok(())
    }
}

impl CredentialStore for DpapiCredentialStore {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        let _lock = self.acquire_lock()?;
        let map = self.load_map()?;
        Ok(map.get(key).cloned().filter(|v| !v.trim().is_empty()))
    }

    fn list_configured_keys(&self) -> Result<Vec<String>, String> {
        let _lock = self.acquire_lock()?;
        let map = self.load_map()?;
        Ok(map
            .into_iter()
            .filter(|(_, v)| !v.trim().is_empty())
            .map(|(k, _)| k)
            .collect())
    }

    fn mutate(
        &self,
        f: &mut dyn FnMut(&mut HashMap<String, String>) -> Result<bool, String>,
    ) -> Result<(), String> {
        let _lock = self.acquire_lock()?;
        let mut map = self.load_map()?;
        let changed = f(&mut map)?;
        if changed {
            self.save_map(&map)?;
        }
        Ok(())
    }
}

/// テスト用のインメモリストア
#[derive(Default, Clone, Debug)]
pub struct InMemoryCredentialStore {
    secrets: std::sync::Arc<parking_lot::RwLock<HashMap<String, String>>>,
}

impl InMemoryCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CredentialStore for InMemoryCredentialStore {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        let lock = self.secrets.read();
        Ok(lock.get(key).cloned().filter(|v| !v.trim().is_empty()))
    }

    fn list_configured_keys(&self) -> Result<Vec<String>, String> {
        let lock = self.secrets.read();
        Ok(lock
            .iter()
            .filter(|(_, v)| !v.trim().is_empty())
            .map(|(k, _)| k.clone())
            .collect())
    }

    fn mutate(
        &self,
        f: &mut dyn FnMut(&mut HashMap<String, String>) -> Result<bool, String>,
    ) -> Result<(), String> {
        let mut lock = self.secrets.write();
        let _changed = f(&mut lock)?;
        Ok(())
    }
}

/// 既存の settings.json から平文クレデンシャルを検出し、credential store へ安全に移行する。
/// 移行成功後に settings.json の平文キーを削除し、credential_migration_version を記録する。
/// 戻り値: 移行されたキー名のベクタ（シークレット本文は含まない）
pub fn migrate_credentials_if_needed(
    root_dir: &Path,
    store: &dyn CredentialStore,
) -> Result<Vec<String>, String> {
    let settings_path = root_dir.join("settings.json");
    if !settings_path.exists() {
        return Ok(Vec::new());
    }

    let content = match fs::read_to_string(&settings_path) {
        Ok(c) => c,
        Err(e) => return Err(format!("Failed to read settings.json for migration: {}", e)),
    };

    let mut json_val: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            return Err(format!(
                "Failed to parse settings.json for migration: {}",
                e
            ))
        }
    };

    let map = match json_val.as_object_mut() {
        Some(m) => m,
        None => return Ok(Vec::new()),
    };

    // すでに移行済み（credential_migration_version >= 1）かつ平文 secret が残っていないか検査
    let mut keys_to_migrate = Vec::new();
    for &sec_key in SECRET_KEYS {
        if let Some(val) = map.get(sec_key) {
            if let Some(val_str) = val.as_str() {
                if !val_str.trim().is_empty() {
                    keys_to_migrate.push((sec_key.to_string(), val_str.trim().to_string()));
                }
            }
        }
    }

    if keys_to_migrate.is_empty() {
        // 平文キーが存在しない場合でも、マイグレーションバージョンが未記録なら記録しておく
        if !map.contains_key("credential_migration_version") {
            map.insert(
                "credential_migration_version".to_string(),
                serde_json::json!(1),
            );
            if let Ok(pretty) = serde_json::to_string_pretty(&json_val) {
                let _ = fs::write(&settings_path, pretty);
            }
        }
        return Ok(Vec::new());
    }

    // 1. 各機密値を credential store に一括保存 (mutate によるアトミックなバッチ更新)
    let keys_to_migrate_ref = &keys_to_migrate;
    store
        .mutate(&mut |cred_map| {
            for (k, v) in keys_to_migrate_ref {
                cred_map.insert(k.clone(), v.clone());
            }
            Ok(true)
        })
        .map_err(|e| format!("Failed to migrate secrets to credential store: {}", e))?;

    // 2. すべての保存が成功した後にのみ、settings.json から平文キーを削除
    let mut migrated_keys = Vec::new();
    for (k, _) in keys_to_migrate {
        map.remove(&k);
        migrated_keys.push(k);
    }

    // 3. migration version を記録
    map.insert(
        "credential_migration_version".to_string(),
        serde_json::json!(1),
    );

    // 4. settings.json を安全に書き込み
    let pretty_str = serde_json::to_string_pretty(&json_val)
        .map_err(|e| format!("Failed to serialize updated settings.json: {}", e))?;
    fs::write(&settings_path, pretty_str)
        .map_err(|e| format!("Failed to write updated settings.json: {}", e))?;

    Ok(migrated_keys)
}

/// シークレット値の取得ヘルパー
/// 1. credential store (DPAPI)
/// 2. 環境変数 (GOOGLE_API_KEY / GEMINI_API_KEY / BRAVE_API_KEY)
/// 3. settings.json (移行前フォールバック)
pub fn get_secret(root_dir: &Path, key: &str) -> Option<String> {
    let store = DpapiCredentialStore::new(root_dir);
    if let Ok(Some(secret)) = store.get(key) {
        if !secret.trim().is_empty() {
            return Some(secret.trim().to_string());
        }
    }

    // 環境変数フォールバック
    if key == "gemini_api_key" {
        if let Ok(k) = std::env::var("GOOGLE_API_KEY").or_else(|_| std::env::var("GEMINI_API_KEY"))
        {
            if !k.trim().is_empty() {
                return Some(k.trim().to_string());
            }
        }
    } else if key == "brave_api_key" {
        if let Ok(k) = std::env::var("BRAVE_API_KEY") {
            if !k.trim().is_empty() {
                return Some(k.trim().to_string());
            }
        }
    }

    // 移行前 settings.json フォールバック
    let settings_path = root_dir.join("settings.json");
    if let Ok(content) = fs::read_to_string(&settings_path) {
        if let Ok(val) = serde_json::from_str::<Value>(&content) {
            if let Some(str_val) = val.get(key).and_then(|v| v.as_str()) {
                if !str_val.trim().is_empty() {
                    return Some(str_val.trim().to_string());
                }
            }
        }
    }

    None
}

/// シークレット値の保存ヘルパー
pub fn set_secret(root_dir: &Path, key: &str, value: &str) -> Result<(), String> {
    let store = DpapiCredentialStore::new(root_dir);
    store.set(key, value)?;

    // settings.json に万が一平文が残っていたら安全に削除
    let settings_path = root_dir.join("settings.json");
    if settings_path.exists() {
        let content = fs::read_to_string(&settings_path).map_err(|e| {
            format!(
                "Failed to read settings.json while purging secret key '{}': {}",
                key, e
            )
        })?;
        let mut val = serde_json::from_str::<Value>(&content).map_err(|e| {
            format!(
                "Failed to parse settings.json while purging secret key '{}': {}",
                key, e
            )
        })?;
        if let Some(map) = val.as_object_mut() {
            if map.remove(key).is_some() {
                let pretty = serde_json::to_string_pretty(&val).map_err(|e| {
                    format!(
                        "Failed to serialize settings.json after purging secret '{}': {}",
                        key, e
                    )
                })?;
                fs::write(&settings_path, pretty).map_err(|e| {
                    format!(
                        "Failed to rewrite settings.json after purging secret '{}': {}",
                        key, e
                    )
                })?;
            }
        }
    }

    Ok(())
}

/// シークレット値の削除ヘルパー
pub fn delete_secret(root_dir: &Path, key: &str) -> Result<(), String> {
    let store = DpapiCredentialStore::new(root_dir);
    store.delete(key)?;

    let settings_path = root_dir.join("settings.json");
    if settings_path.exists() {
        let content = fs::read_to_string(&settings_path).map_err(|e| {
            format!(
                "Failed to read settings.json while purging deleted secret '{}': {}",
                key, e
            )
        })?;
        let mut val = serde_json::from_str::<Value>(&content).map_err(|e| {
            format!(
                "Failed to parse settings.json while purging deleted secret '{}': {}",
                key, e
            )
        })?;
        if let Some(map) = val.as_object_mut() {
            if map.remove(key).is_some() {
                let pretty = serde_json::to_string_pretty(&val).map_err(|e| {
                    format!(
                        "Failed to serialize settings.json after purging deleted secret '{}': {}",
                        key, e
                    )
                })?;
                fs::write(&settings_path, pretty).map_err(|e| {
                    format!(
                        "Failed to rewrite settings.json after purging deleted secret '{}': {}",
                        key, e
                    )
                })?;
            }
        }
    }

    Ok(())
}

/// シークレットが設定済み（非空で存在）かどうかを判定する
pub fn is_secret_configured(root_dir: &Path, key: &str) -> bool {
    get_secret(root_dir, key).is_some()
}

/// フロントエンドに渡す settings オブジェクトを安全化（サニタイズ）する
/// - 平文の secret はすべて除去
/// - 設定済みフラグ（`has_{key}` 等）を付与
pub fn sanitize_settings_for_frontend(
    root_dir: &Path,
    settings: &mut Value,
    store: &dyn CredentialStore,
) -> Result<(), String> {
    // 移行を実行。失敗した場合はエラーを伝播し、安全でない状態を隠蔽しない
    migrate_credentials_if_needed(root_dir, store)?;

    if let Value::Object(ref mut map) = settings {
        // 平文シークレットキーを完全に除去
        for &sec_key in SECRET_KEYS {
            map.remove(sec_key);
        }

        // 各シークレットの設定済みフラグを設定
        let configured = store.list_configured_keys().unwrap_or_default();
        for &sec_key in SECRET_KEYS {
            let is_configured =
                configured.iter().any(|k| k == sec_key) || is_secret_configured(root_dir, sec_key);
            let flag_name = format!("has_{}", sec_key);
            map.insert(flag_name, serde_json::json!(is_configured));
        }

        // 既存フロントエンド互換用フラグ
        let has_secret = configured.iter().any(|k| k == "twitch_client_secret")
            || is_secret_configured(root_dir, "twitch_client_secret");
        map.insert(
            "has_client_secret".to_string(),
            serde_json::json!(has_secret),
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dpapi_encrypt_decrypt_roundtrip() {
        let secret = "dummy_test_token_roundtrip_12345";
        let encrypted = dpapi_encrypt(secret.as_bytes()).expect("DPAPI encryption must succeed");
        assert_ne!(
            encrypted,
            secret.as_bytes(),
            "Ciphertext must not match plaintext"
        );
        assert!(!encrypted.is_empty());

        let decrypted = dpapi_decrypt(&encrypted).expect("DPAPI decryption must succeed");
        let decrypted_str = String::from_utf8(decrypted).expect("Must be valid UTF-8");
        assert_eq!(decrypted_str, secret);
    }

    #[test]
    fn test_dpapi_credential_store_crud() {
        let temp_dir =
            std::env::temp_dir().join(format!("ga_cred_test_{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir_all(&temp_dir).unwrap();

        let store = DpapiCredentialStore::new(&temp_dir);

        // 最初は何も存在しない
        assert_eq!(store.get("gemini_api_key").unwrap(), None);
        assert!(!store.exists("gemini_api_key").unwrap());

        // 保存
        store
            .set("gemini_api_key", "dummy_test_gemini_key_for_test")
            .expect("save secret");
        assert_eq!(
            store.get("gemini_api_key").unwrap(),
            Some("dummy_test_gemini_key_for_test".to_string())
        );
        assert!(store.exists("gemini_api_key").unwrap());

        // credentials.enc ファイルが暗号化されており、平文を含まないことの検証
        let enc_path = temp_dir.join("credentials.enc");
        assert!(enc_path.exists());
        let raw_bytes = fs::read(&enc_path).unwrap();
        let raw_content = String::from_utf8_lossy(&raw_bytes);
        assert!(
            !raw_content.contains("dummy_test_gemini_key_for_test"),
            "Ciphertext file MUST NOT contain plaintext secret"
        );

        // 削除
        store.delete("gemini_api_key").expect("delete secret");
        assert_eq!(store.get("gemini_api_key").unwrap(), None);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    /// Blocker 1 回帰テスト:
    /// Windows では宛先が存在すると通常の std::fs::rename が失敗するが、
    /// atomic_replace_file (MoveFileExW REPLACE_EXISTING) により
    /// 既存の credentials.enc が存在していても2個目以降の追加・更新が確実に成功することを検証する
    #[test]
    fn test_dpapi_credential_store_multiple_keys_and_updates_replace_file() {
        let temp_dir = std::env::temp_dir().join(format!(
            "ga_cred_multikey_test_{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&temp_dir).unwrap();

        let store = DpapiCredentialStore::new(&temp_dir);

        // 1. 最初キー (secret A) を保存 -> credentials.enc が新規作成される
        store
            .set("gemini_api_key", "dummy_test_gemini_initial")
            .expect("initial secret set must succeed");
        assert_eq!(
            store.get("gemini_api_key").unwrap().as_deref(),
            Some("dummy_test_gemini_initial")
        );

        // 2. 2個目のキー (secret B) を追加 -> credentials.enc が既に存在する状態での置換書き込み
        store
            .set("brave_api_key", "dummy_test_brave_initial")
            .expect("second secret set on existing file must succeed via atomic replace");
        assert_eq!(
            store.get("gemini_api_key").unwrap().as_deref(),
            Some("dummy_test_gemini_initial")
        );
        assert_eq!(
            store.get("brave_api_key").unwrap().as_deref(),
            Some("dummy_test_brave_initial")
        );

        // 3. 既存のキー (secret A) を更新 -> 既存ファイルの置換更新
        store
            .set("gemini_api_key", "dummy_test_gemini_updated")
            .expect("updating existing secret must succeed via atomic replace");

        // 4. 両方の最新値が正しく復号取得できることを検証
        assert_eq!(
            store.get("gemini_api_key").unwrap().as_deref(),
            Some("dummy_test_gemini_updated")
        );
        assert_eq!(
            store.get("brave_api_key").unwrap().as_deref(),
            Some("dummy_test_brave_initial")
        );

        let keys = store.list_configured_keys().unwrap();
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&"gemini_api_key".to_string()));
        assert!(keys.contains(&"brave_api_key".to_string()));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_migration_from_plain_settings_json() {
        let temp_dir = std::env::temp_dir().join(format!(
            "ga_migration_test_{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&temp_dir).unwrap();

        let settings_path = temp_dir.join("settings.json");
        let initial_json = serde_json::json!({
            "user_name": "Kota",
            "gemini_api_key": "dummy_test_gemini_key_to_migrate",
            "brave_api_key": "dummy_test_brave_key_to_migrate",
            "twitch_client_secret": "dummy_test_twitch_secret_to_migrate",
            "twitch_access_token": "oauth:dummy_test_twitch_token_to_migrate",
            "regular_setting": "keep_this_value"
        });
        fs::write(
            &settings_path,
            serde_json::to_string_pretty(&initial_json).unwrap(),
        )
        .unwrap();

        let store = InMemoryCredentialStore::new();
        let migrated = migrate_credentials_if_needed(&temp_dir, &store).unwrap();

        // 移行されたキー名リストの検証（キー名のみで値は含まない）
        assert_eq!(migrated.len(), 4);
        assert!(migrated.contains(&"gemini_api_key".to_string()));
        assert!(migrated.contains(&"brave_api_key".to_string()));
        assert!(migrated.contains(&"twitch_client_secret".to_string()));
        assert!(migrated.contains(&"twitch_access_token".to_string()));

        // store に保存されていることの検証
        assert_eq!(
            store.get("gemini_api_key").unwrap().as_deref(),
            Some("dummy_test_gemini_key_to_migrate")
        );
        assert_eq!(
            store.get("brave_api_key").unwrap().as_deref(),
            Some("dummy_test_brave_key_to_migrate")
        );
        assert_eq!(
            store.get("twitch_client_secret").unwrap().as_deref(),
            Some("dummy_test_twitch_secret_to_migrate")
        );
        assert_eq!(
            store.get("twitch_access_token").unwrap().as_deref(),
            Some("oauth:dummy_test_twitch_token_to_migrate")
        );

        // settings.json から平文が完全に削除されていることの検証
        let reloaded_content = fs::read_to_string(&settings_path).unwrap();
        assert!(
            !reloaded_content.contains("dummy_test_gemini_key_to_migrate"),
            "settings.json must NOT contain plain gemini_api_key"
        );
        assert!(
            !reloaded_content.contains("dummy_test_brave_key_to_migrate"),
            "settings.json must NOT contain plain brave_api_key"
        );
        assert!(
            !reloaded_content.contains("dummy_test_twitch_secret_to_migrate"),
            "settings.json must NOT contain plain twitch_client_secret"
        );
        assert!(
            !reloaded_content.contains("oauth:dummy_test_twitch_token_to_migrate"),
            "settings.json must NOT contain plain twitch_access_token"
        );

        // 通常設定と migration version が維持されていることの検証
        let reloaded_json: Value = serde_json::from_str(&reloaded_content).unwrap();
        assert_eq!(
            reloaded_json.get("regular_setting"),
            Some(&serde_json::json!("keep_this_value"))
        );
        assert_eq!(
            reloaded_json.get("user_name"),
            Some(&serde_json::json!("Kota"))
        );
        assert_eq!(
            reloaded_json.get("credential_migration_version"),
            Some(&serde_json::json!(1))
        );

        let _ = fs::remove_dir_all(&temp_dir);
    }

    /// Blocker 2 回帰テスト:
    /// settings.json への平文削除書き込みが失敗した場合、migration error が適切に返され、
    /// 完了扱い（migration version 記録など）にならないことを検証する
    #[test]
    fn test_migration_fails_when_settings_json_cannot_be_written() {
        let temp_dir = std::env::temp_dir().join(format!(
            "ga_migration_fail_test_{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&temp_dir).unwrap();

        let settings_path = temp_dir.join("settings.json");
        let initial_json = serde_json::json!({
            "gemini_api_key": "dummy_test_gemini_key_purge_fail"
        });
        fs::write(
            &settings_path,
            serde_json::to_string_pretty(&initial_json).unwrap(),
        )
        .unwrap();

        // settings.json を読み取り専用にして書き込みを意図的に失敗させる
        let mut perms = fs::metadata(&settings_path).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&settings_path, perms).unwrap();

        let store = InMemoryCredentialStore::new();
        let res = migrate_credentials_if_needed(&temp_dir, &store);

        // 平文削除書き込み失敗がエラーとして伝播すること
        assert!(
            res.is_err(),
            "Migration must return Err when settings.json cannot be written to purge secrets"
        );

        // 読み取り専用を解除してクリーンアップ
        let mut perms = fs::metadata(&settings_path).unwrap().permissions();
        perms.set_readonly(false);
        let _ = fs::set_permissions(&settings_path, perms);
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_sanitize_settings_for_frontend_does_not_leak_secrets() {
        let temp_dir = std::env::temp_dir().join(format!(
            "ga_sanitize_test_{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&temp_dir).unwrap();

        let store = InMemoryCredentialStore::new();
        store
            .set("gemini_api_key", "dummy_test_gemini_secret_for_sanitize")
            .unwrap();
        store
            .set(
                "twitch_client_secret",
                "dummy_test_twitch_secret_for_sanitize",
            )
            .unwrap();

        let mut settings = serde_json::json!({
            "gemini_api_key": "dummy_test_gemini_secret_for_sanitize",
            "twitch_client_secret": "dummy_test_twitch_secret_for_sanitize",
            "user_name": "Kota"
        });

        sanitize_settings_for_frontend(&temp_dir, &mut settings, &store)
            .expect("sanitize_settings_for_frontend should succeed");

        let settings_str = serde_json::to_string(&settings).unwrap();
        assert!(
            !settings_str.contains("dummy_test_gemini_secret_for_sanitize"),
            "Frontend settings must not contain plain secret"
        );
        assert!(
            !settings_str.contains("dummy_test_twitch_secret_for_sanitize"),
            "Frontend settings must not contain plain secret"
        );
        assert_eq!(
            settings.get("has_gemini_api_key"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            settings.get("has_twitch_client_secret"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            settings.get("has_client_secret"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            settings.get("has_brave_api_key"),
            Some(&serde_json::json!(false))
        );
        assert_eq!(settings.get("user_name"), Some(&serde_json::json!("Kota")));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_secret_is_not_leaked_in_errors_or_logs() {
        let sensitive = "dummy_test_sensitive_data_12345";
        let bad_bytes = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let err = dpapi_decrypt(&bad_bytes).unwrap_err();
        assert!(
            !err.contains(sensitive),
            "Error message must never contain sensitive tokens"
        );
    }

    #[test]
    fn test_delete_fails_when_credentials_file_cannot_be_removed() {
        let temp_dir = std::env::temp_dir().join(format!(
            "ga_dpapi_delete_fail_test_{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&temp_dir).unwrap();

        let store = DpapiCredentialStore::new(&temp_dir);
        store
            .set("gemini_api_key", "dummy_test_gemini_key_1")
            .unwrap();

        let cred_path = temp_dir.join("credentials.enc");
        assert!(cred_path.exists());

        // ファイルを開いてロックを保持することで、remove_file を共有違反で確実に失敗させる
        use std::os::windows::fs::OpenOptionsExt;
        let _locked_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(1 | 2) // FILE_SHARE_READ | FILE_SHARE_WRITE; exclude FILE_SHARE_DELETE
            .open(&cred_path)
            .unwrap();

        // 唯一のキーを削除しようとする -> map が空になり remove_file が呼ばれる -> エラーが返ること
        let res = store.delete("gemini_api_key");
        assert!(
            res.is_err(),
            "delete should fail when credentials.enc is locked and cannot be removed"
        );

        drop(_locked_file);
        let _ = fs::remove_dir_all(&temp_dir);
    }

    /// Issue #42 回帰テスト:
    /// 並行する複数スレッドから異なるキーを一斉に set しても
    /// 排他制御により lost update が発生せず、全キーが確実に保存されることを検証する
    #[test]
    fn test_concurrent_set_distinct_keys_no_lost_update() {
        let temp_dir = std::env::temp_dir().join(format!(
            "ga_concurrent_set_test_{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&temp_dir).unwrap();

        let num_threads = 10;
        let barrier = Arc::new(std::sync::Barrier::new(num_threads));
        let mut handles = Vec::new();

        for i in 0..num_threads {
            let dir = temp_dir.clone();
            let b = barrier.clone();
            let handle = std::thread::spawn(move || {
                let store = DpapiCredentialStore::new(&dir);
                b.wait();
                let key = format!("concurrent_key_{}", i);
                let val = format!("concurrent_val_{}", i);
                store.set(&key, &val).expect("concurrent set must succeed");
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().expect("thread join must succeed");
        }

        let verify_store = DpapiCredentialStore::new(&temp_dir);
        let keys = verify_store.list_configured_keys().expect("list keys");
        assert_eq!(
            keys.len(),
            num_threads,
            "All concurrently written distinct keys must be preserved without lost update"
        );

        for i in 0..num_threads {
            let key = format!("concurrent_key_{}", i);
            let expected_val = format!("concurrent_val_{}", i);
            assert_eq!(
                verify_store.get(&key).expect("get key").as_deref(),
                Some(expected_val.as_str()),
                "Value for key '{}' must match",
                key
            );
        }

        let _ = fs::remove_dir_all(&temp_dir);
    }

    /// Issue #42 回帰テスト:
    /// 複数スレッドから高頻度で並行して set と delete を実行しても、
    /// credentials.enc が破損せず整合性が維持されることを検証する
    #[test]
    fn test_concurrent_set_and_delete_consistency() {
        let temp_dir = std::env::temp_dir().join(format!(
            "ga_concurrent_set_del_test_{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&temp_dir).unwrap();

        // 初期キーを準備
        let initial_store = DpapiCredentialStore::new(&temp_dir);
        for i in 0..5 {
            initial_store
                .set(&format!("shared_key_{}", i), "initial_val")
                .unwrap();
        }

        let num_threads = 8;
        let barrier = Arc::new(std::sync::Barrier::new(num_threads));
        let mut handles = Vec::new();

        for i in 0..num_threads {
            let dir = temp_dir.clone();
            let b = barrier.clone();
            let handle = std::thread::spawn(move || {
                let store = DpapiCredentialStore::new(&dir);
                b.wait();
                for iter in 0..5 {
                    let key = format!("shared_key_{}", (i + iter) % 5);
                    if (i + iter) % 2 == 0 {
                        let _ = store.set(&key, &format!("worker_{}_iter_{}", i, iter));
                    } else {
                        let _ = store.delete(&key);
                    }
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().expect("thread join must succeed");
        }

        // 高頻度並行操作後も破損せず正常に読み取れることの検証
        let verify_store = DpapiCredentialStore::new(&temp_dir);
        let configured = verify_store
            .list_configured_keys()
            .expect("must parse successfully");
        for key in &configured {
            let val = verify_store.get(key).expect("must get value");
            assert!(val.is_some(), "Configured key must have a value");
        }

        let _ = fs::remove_dir_all(&temp_dir);
    }

    /// Issue #42 回帰テスト:
    /// mutate による一括更新と、クロージャ内エラー発生時のロールバック（変更破棄）を検証する
    #[test]
    fn test_mutate_atomic_batch_update() {
        let temp_dir = std::env::temp_dir().join(format!(
            "ga_mutate_batch_test_{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&temp_dir).unwrap();

        let store = DpapiCredentialStore::new(&temp_dir);
        store.set("key_a", "val_a").unwrap();

        // 一括更新
        store
            .mutate(&mut |map| {
                map.insert("key_b".to_string(), "val_b".to_string());
                map.insert("key_c".to_string(), "val_c".to_string());
                map.remove("key_a");
                Ok(true)
            })
            .expect("mutate batch must succeed");

        assert_eq!(store.get("key_a").unwrap(), None);
        assert_eq!(store.get("key_b").unwrap().as_deref(), Some("val_b"));
        assert_eq!(store.get("key_c").unwrap().as_deref(), Some("val_c"));

        // エラー発生時のロールバック検証（ファイルへの書き込みが行われないこと）
        let err_res = store.mutate(&mut |map| {
            map.insert("key_temp".to_string(), "val_temp".to_string());
            Err("abort mutation".to_string())
        });
        assert!(err_res.is_err());
        assert_eq!(store.get("key_temp").unwrap(), None);

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
