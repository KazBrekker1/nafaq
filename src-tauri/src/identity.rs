use anyhow::{Context, Result};
use iroh::SecretKey;
use tauri::Runtime;
use tauri_plugin_store::{Store, StoreExt};

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IdentityStatus {
    LoadedPersistent,
    CreatedPersistent,
    ResetRequired,
}

#[derive(Debug, Clone)]
pub struct LoadedIdentity {
    pub secret_key: SecretKey,
    pub status: IdentityStatus,
}

const SETTINGS_FILE: &str = "settings.json";
const SECRET_KEY_KEY: &str = "secret_key";
const PERSISTENT_KEY: &str = "persistent_identity";

pub fn load_or_create_persistent_identity<R: Runtime>(
    app: &tauri::AppHandle<R>,
) -> Result<LoadedIdentity> {
    let store = app.store(SETTINGS_FILE)?;
    load_or_create_from_store(&store)
}

fn load_or_create_from_store<R: Runtime>(store: &Store<R>) -> Result<LoadedIdentity> {
    let persistent = store
        .get(PERSISTENT_KEY)
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let stored_secret_key = match store.get(SECRET_KEY_KEY) {
        Some(value) => Some(
            value
                .as_str()
                .map(String::from)
                .context("stored node identity is invalid: secret_key must be a string")?,
        ),
        None => None,
    };

    match stored_secret_key {
        Some(raw) => {
            let key = parse_secret_key(&raw).context("stored node identity is invalid")?;
            if !persistent {
                store.set(PERSISTENT_KEY, serde_json::Value::Bool(true));
                store.save()?;
            }
            Ok(LoadedIdentity {
                secret_key: key,
                status: IdentityStatus::LoadedPersistent,
            })
        }
        None if persistent => anyhow::bail!(
            "persistent node identity is enabled but no secret key was found; explicit reset required"
        ),
        None => {
            let mut rng = rand::rng();
            let key = SecretKey::generate(&mut rng);
            persist_secret_key(store, &key)?;
            Ok(LoadedIdentity {
                secret_key: key,
                status: IdentityStatus::CreatedPersistent,
            })
        }
    }
}

fn parse_secret_key(raw: &str) -> Result<SecretKey> {
    let raw = raw.trim();

    if let Ok(key) = raw.parse::<SecretKey>() {
        return Ok(key);
    }

    if raw.len() == 64 && raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        let mut bytes = [0_u8; 32];
        for (idx, chunk) in raw.as_bytes().chunks_exact(2).enumerate() {
            let hex = std::str::from_utf8(chunk)?;
            bytes[idx] = u8::from_str_radix(hex, 16)?;
        }
        return Ok(SecretKey::from_bytes(&bytes));
    }

    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(raw)?;
    let bytes: [u8; 32] = bytes.as_slice().try_into()?;
    Ok(SecretKey::from_bytes(&bytes))
}

pub fn persist_secret_key<R: Runtime>(store: &Store<R>, key: &SecretKey) -> Result<()> {
    let hex: String = key.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
    store.set(SECRET_KEY_KEY, serde_json::Value::String(hex));
    store.set(PERSISTENT_KEY, serde_json::Value::Bool(true));
    store.save()?;
    Ok(())
}

pub fn status_from_store<R: Runtime>(
    store: &Store<R>,
    loaded_status: &IdentityStatus,
) -> IdentityStatus {
    let persistent = store
        .get(PERSISTENT_KEY)
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let has_secret_key = store.get(SECRET_KEY_KEY).is_some();

    if persistent && !has_secret_key {
        IdentityStatus::ResetRequired
    } else {
        loaded_status.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tauri_plugin_store::StoreExt;

    fn test_store() -> (tauri::App<tauri::test::MockRuntime>, PathBuf) {
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_store::Builder::new().build())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app should build");
        let dir =
            std::env::temp_dir().join(format!("nafaq-identity-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("test temp dir should be created");
        (app, dir.join("settings.json"))
    }

    #[test]
    fn missing_key_without_flag_creates_and_stores_persistent_key() {
        let (app, path) = test_store();
        let store = app
            .store_builder(&path)
            .disable_auto_save()
            .build()
            .expect("store should build");

        let loaded = load_or_create_from_store(&store).expect("identity should be created");

        assert_eq!(loaded.status, IdentityStatus::CreatedPersistent);
        assert_eq!(
            store.get(PERSISTENT_KEY).and_then(|v| v.as_bool()),
            Some(true)
        );
        let stored = store
            .get(SECRET_KEY_KEY)
            .and_then(|v| v.as_str().map(String::from))
            .expect("secret key should be persisted");
        let reparsed = parse_secret_key(&stored).expect("stored key should parse");
        assert_eq!(loaded.secret_key.public(), reparsed.public());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn existing_key_loads_same_identity_and_enables_persistence_flag() {
        let (app, path) = test_store();
        let store = app
            .store_builder(&path)
            .disable_auto_save()
            .build()
            .expect("store should build");
        let key = SecretKey::from_bytes(&[7_u8; 32]);
        let hex: String = key.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
        store.set(SECRET_KEY_KEY, serde_json::Value::String(hex));
        store.save().expect("store should save");

        let loaded = load_or_create_from_store(&store).expect("identity should load");

        assert_eq!(loaded.status, IdentityStatus::LoadedPersistent);
        assert_eq!(loaded.secret_key.public(), key.public());
        assert_eq!(
            store.get(PERSISTENT_KEY).and_then(|v| v.as_bool()),
            Some(true)
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn persistent_flag_without_key_returns_explicit_error() {
        let (app, path) = test_store();
        let store = app
            .store_builder(&path)
            .disable_auto_save()
            .build()
            .expect("store should build");
        store.set(PERSISTENT_KEY, serde_json::Value::Bool(true));
        store.save().expect("store should save");

        let err = load_or_create_from_store(&store).expect_err("missing key should error");

        assert!(err.to_string().contains("explicit reset required"));
        assert!(store.get(SECRET_KEY_KEY).is_none());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn invalid_stored_key_returns_error() {
        let (app, path) = test_store();
        let store = app
            .store_builder(&path)
            .disable_auto_save()
            .build()
            .expect("store should build");
        store.set(PERSISTENT_KEY, serde_json::Value::Bool(true));
        store.set(
            SECRET_KEY_KEY,
            serde_json::Value::String("not-a-valid-secret-key".to_string()),
        );
        store.save().expect("store should save");

        let err = load_or_create_from_store(&store).expect_err("invalid key should error");

        assert!(err.to_string().contains("stored node identity is invalid"));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn non_string_stored_key_returns_error_without_persistence_flag() {
        let (app, path) = test_store();
        let store = app
            .store_builder(&path)
            .disable_auto_save()
            .build()
            .expect("store should build");
        store.set(SECRET_KEY_KEY, serde_json::Value::Bool(false));
        store.save().expect("store should save");

        let err = load_or_create_from_store(&store).expect_err("non-string key should error");

        assert!(err.to_string().contains("stored node identity is invalid"));
        assert!(err.to_string().contains("secret_key must be a string"));
        assert_eq!(
            store.get(SECRET_KEY_KEY),
            Some(serde_json::Value::Bool(false))
        );
        assert!(store.get(PERSISTENT_KEY).is_none());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn non_string_stored_key_returns_error_with_false_persistence_flag() {
        let (app, path) = test_store();
        let store = app
            .store_builder(&path)
            .disable_auto_save()
            .build()
            .expect("store should build");
        store.set(PERSISTENT_KEY, serde_json::Value::Bool(false));
        store.set(SECRET_KEY_KEY, serde_json::Value::Number(1.into()));
        store.save().expect("store should save");

        let err = load_or_create_from_store(&store).expect_err("non-string key should error");

        assert!(err.to_string().contains("stored node identity is invalid"));
        assert!(err.to_string().contains("secret_key must be a string"));
        assert_eq!(
            store.get(SECRET_KEY_KEY),
            Some(serde_json::Value::Number(1.into()))
        );
        assert_eq!(
            store.get(PERSISTENT_KEY).and_then(|v| v.as_bool()),
            Some(false)
        );

        let _ = std::fs::remove_file(path);
    }
}
