//! OS credential store for the device key, `install_id` and the two-slot
//! refresh chain. Real backend: Windows Credential Manager via `keyring`
//! `windows-native` (contract v5 section 1). No plaintext fallback and no
//! mock production backend — an unsupported platform is a hard
//! [`StoreError::UnsupportedPlatform`].
use super::traits::{ClientProfile, SecretStore, StoreError, StoredProfileData};
use keyring::Entry;

const SERVICE_PREFIX: &str = "rocks.tfd.bridge.account";

/// Conservative bound, well under the classic Windows Credential Manager
/// generic-credential blob limit (`CRED_MAX_CREDENTIAL_BLOB_SIZE`, 2560
/// bytes / 5*512). Checked before every write so a future schema addition
/// fails loudly here instead of silently at the platform boundary. The
/// platform's own limit is still respected as a backstop: `keyring::Error::
/// TooLong` maps to the same `StoreError::ValueTooLarge` below.
const MAX_CREDENTIAL_BYTES: usize = 2000;

pub struct KeyringStore;

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyringStore {
    pub fn new() -> Self {
        Self
    }

    fn entry(&self, username: &str, profile_suffix: Option<&str>) -> Result<Entry, StoreError> {
        let service = match profile_suffix {
            Some(p) => format!("{SERVICE_PREFIX}.{p}"),
            None => SERVICE_PREFIX.to_string(),
        };
        Entry::new(&service, username).map_err(map_backend_error)
    }
}

fn map_backend_error(e: keyring::Error) -> StoreError {
    match e {
        keyring::Error::PlatformFailure(_) | keyring::Error::NoStorageAccess(_) => {
            StoreError::UnsupportedPlatform
        }
        keyring::Error::TooLong(..) => StoreError::ValueTooLarge,
        keyring::Error::BadEncoding(_) => StoreError::Corrupt,
        other => StoreError::Backend(other.to_string()),
    }
}

impl SecretStore for KeyringStore {
    fn install_id(&self) -> Result<String, StoreError> {
        let entry = self.entry("install_id", None)?;
        match entry.get_password() {
            Ok(value) if !value.is_empty() => Ok(value),
            Ok(_) | Err(keyring::Error::NoEntry) => {
                let mut bytes = [0u8; 16];
                use rand::RngCore;
                rand::rngs::OsRng.fill_bytes(&mut bytes);
                // Reuses the same UUIDv4-shaping helper the DPoP `jti` claim
                // uses; here it just needs a stable, opaque per-install id.
                let id = bridge_core::dpop::jti_from_bytes(bytes);
                entry.set_password(&id).map_err(map_backend_error)?;
                Ok(id)
            }
            Err(e) => Err(map_backend_error(e)),
        }
    }

    fn load_profile(&self, profile: ClientProfile) -> Result<Option<StoredProfileData>, StoreError> {
        let entry = self.entry("profile", Some(profile.as_str()))?;
        match entry.get_password() {
            Ok(json) => {
                let data: StoredProfileData =
                    serde_json::from_str(&json).map_err(|_| StoreError::Corrupt)?;
                Ok(Some(data))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_backend_error(e)),
        }
    }

    fn save_profile(&self, profile: ClientProfile, data: &StoredProfileData) -> Result<(), StoreError> {
        let json = serde_json::to_string(data).map_err(|_| StoreError::Corrupt)?;
        if json.len() > MAX_CREDENTIAL_BYTES {
            return Err(StoreError::ValueTooLarge);
        }
        let entry = self.entry("profile", Some(profile.as_str()))?;
        entry.set_password(&json).map_err(map_backend_error)
    }

    fn clear_profile(&self, profile: ClientProfile) -> Result<(), StoreError> {
        let entry = self.entry("profile", Some(profile.as_str()))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(map_backend_error(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::traits::{StoredAccount, StoredRefresh, STORED_PROFILE_SCHEMA_VERSION};

    // These tests hit the REAL Windows Credential Manager (no mock backend
    // exists for production code) using a per-test-run unique service
    // suffix, and clean up after themselves. They are Windows-only in
    // practice (the workspace only ships/validates on Windows — see
    // AGENTS.md) but compile everywhere via `keyring`'s own platform gating.
    fn unique_store() -> KeyringStoreForTest {
        let suffix = format!(
            "{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        KeyringStoreForTest(suffix)
    }

    /// Same behaviour as `KeyringStore`, but every entry is namespaced with a
    /// per-test-run suffix so parallel test runs and repeated runs never
    /// collide on the same real credential-store entries.
    struct KeyringStoreForTest(String);
    impl KeyringStoreForTest {
        fn entry(&self, username: &str, profile_suffix: Option<&str>) -> Entry {
            let service = match profile_suffix {
                Some(p) => format!("{SERVICE_PREFIX}.test.{}.{p}", self.0),
                None => format!("{SERVICE_PREFIX}.test.{}", self.0),
            };
            Entry::new(&service, username).unwrap()
        }
    }
    impl SecretStore for KeyringStoreForTest {
        fn install_id(&self) -> Result<String, StoreError> {
            let entry = self.entry("install_id", None);
            match entry.get_password() {
                Ok(v) if !v.is_empty() => Ok(v),
                _ => {
                    let id = "test-install-id".to_string();
                    entry.set_password(&id).map_err(map_backend_error)?;
                    Ok(id)
                }
            }
        }
        fn load_profile(&self, profile: ClientProfile) -> Result<Option<StoredProfileData>, StoreError> {
            let entry = self.entry("profile", Some(profile.as_str()));
            match entry.get_password() {
                Ok(json) => Ok(Some(
                    serde_json::from_str(&json).map_err(|_| StoreError::Corrupt)?,
                )),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(map_backend_error(e)),
            }
        }
        fn save_profile(&self, profile: ClientProfile, data: &StoredProfileData) -> Result<(), StoreError> {
            let json = serde_json::to_string(data).unwrap();
            if json.len() > MAX_CREDENTIAL_BYTES {
                return Err(StoreError::ValueTooLarge);
            }
            self.entry("profile", Some(profile.as_str()))
                .set_password(&json)
                .map_err(map_backend_error)
        }
        fn clear_profile(&self, profile: ClientProfile) -> Result<(), StoreError> {
            match self.entry("profile", Some(profile.as_str())).delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(map_backend_error(e)),
            }
        }
    }

    fn sample_profile() -> StoredProfileData {
        StoredProfileData {
            schema_version: STORED_PROFILE_SCHEMA_VERSION,
            device_key_scalar_b64: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAE".into(),
            device_id: Some("dev_test".into()),
            refresh_current: Some(StoredRefresh {
                refresh_token: "refresh-current".into(),
                refresh_expires_at_unix: 1,
                refresh_absolute_expires_at_unix: 2,
                account: StoredAccount {
                    account_id: None,
                    nickname: "Tester".into(),
                    clan_tag: Some("TFX".into()),
                    realm: Some("eu".into()),
                },
            }),
            refresh_pending: None,
        }
    }

    #[test]
    fn install_id_is_created_once_and_then_stable() {
        let store = unique_store();
        let first = store.install_id().unwrap();
        assert!(!first.is_empty());
        let second = store.install_id().unwrap();
        assert_eq!(first, second);
        store.clear_profile(ClientProfile::Release).ok();
        let entry = store.entry("install_id", None);
        entry.delete_credential().ok();
    }

    #[test]
    fn profile_round_trips_and_release_debug_are_isolated_namespaces() {
        let store = unique_store();
        assert_eq!(store.load_profile(ClientProfile::Release).unwrap(), None);
        assert_eq!(store.load_profile(ClientProfile::Debug).unwrap(), None);

        store.save_profile(ClientProfile::Release, &sample_profile()).unwrap();
        assert_eq!(
            store.load_profile(ClientProfile::Release).unwrap(),
            Some(sample_profile())
        );
        // The debug namespace must still be untouched by a release write.
        assert_eq!(store.load_profile(ClientProfile::Debug).unwrap(), None);

        store.clear_profile(ClientProfile::Release).unwrap();
        assert_eq!(store.load_profile(ClientProfile::Release).unwrap(), None);
    }

    #[test]
    fn clear_profile_is_idempotent_when_nothing_is_stored() {
        let store = unique_store();
        assert!(store.clear_profile(ClientProfile::Release).is_ok());
        assert!(store.clear_profile(ClientProfile::Release).is_ok());
    }

    #[test]
    fn corrupt_stored_json_is_reported_not_panicked_on() {
        let store = unique_store();
        store
            .entry("profile", Some(ClientProfile::Release.as_str()))
            .set_password("not valid json")
            .unwrap();
        assert_eq!(
            store.load_profile(ClientProfile::Release),
            Err(StoreError::Corrupt)
        );
        store.clear_profile(ClientProfile::Release).ok();
    }

    #[test]
    fn oversized_value_is_rejected_before_touching_the_store() {
        let store = unique_store();
        let mut profile = sample_profile();
        profile.device_key_scalar_b64 = "x".repeat(MAX_CREDENTIAL_BYTES);
        assert_eq!(
            store.save_profile(ClientProfile::Release, &profile),
            Err(StoreError::ValueTooLarge)
        );
        // Nothing was written — a normal-sized profile still loads as absent.
        assert_eq!(store.load_profile(ClientProfile::Release).unwrap(), None);
    }
}
