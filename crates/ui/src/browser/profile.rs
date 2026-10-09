//! Opaque browser-profile storage keyed by the workspace locator.
use std::path::{Path, PathBuf};

const LOCATOR_LEN: usize = 16;

fn validate_locator(locator: &str) -> Result<(), String> {
    if locator.len() != LOCATOR_LEN
        || !locator
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f' | 'A'..='F'))
    {
        return Err("Workspace locator must be 16 hexadecimal characters".into());
    }
    Ok(())
}

/// Stable on-disk browser partition for one Zeron workspace identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserProfileStorage {
    /// Workspace locator (16 hex chars); never contains raw credentials.
    pub locator: String,
    root: PathBuf,
}

impl BrowserProfileStorage {
    pub fn from_locator(data_dir: &Path, locator: &str) -> Result<Self, String> {
        validate_locator(locator)?;
        Ok(Self {
            locator: locator.to_owned(),
            root: data_dir.join("browser").join(locator),
        })
    }

    pub fn storage_root(&self) -> &Path {
        &self.root
    }

    pub fn webkit_data_dir(&self) -> PathBuf {
        self.root.join("webkit-data")
    }

    pub fn webkit_cache_dir(&self) -> PathBuf {
        self.root.join("webkit-cache")
    }

    pub fn webview2_dir(&self) -> PathBuf {
        self.root.join("webview2")
    }

    pub fn windows_profile_name(&self) -> String {
        format!("zeron-{}", self.locator)
    }

    /// Deterministic store id for WKWebsiteDataStore / WebView2 isolation.
    pub fn store_uuid(&self) -> uuid::Uuid {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(format!("zeron-browser-store\0{}", self.locator).as_bytes());
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        uuid::Uuid::from_bytes(bytes)
    }

    pub fn ensure_directories(&self) -> std::io::Result<()> {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(self.storage_root())?;
        #[cfg(unix)]
        std::fs::set_permissions(self.storage_root(), std::fs::Permissions::from_mode(0o700))?;
        for dir in [
            self.webkit_data_dir(),
            self.webkit_cache_dir(),
            self.webview2_dir(),
        ] {
            std::fs::create_dir_all(&dir)?;
            #[cfg(unix)]
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowserProfileMode {
    /// No workspace identity yet — native stores stay ephemeral.
    Deferred,
    Persistent(BrowserProfileStorage),
}

impl Default for BrowserProfileMode {
    fn default() -> Self {
        Self::Deferred
    }
}

impl BrowserProfileMode {
    pub fn persistent(data_dir: &Path, locator: &str) -> Result<Self, String> {
        Ok(Self::Persistent(BrowserProfileStorage::from_locator(
            data_dir, locator,
        )?))
    }

    pub fn is_persistent(&self) -> bool {
        matches!(self, Self::Persistent(_))
    }

    pub fn storage(&self) -> Option<&BrowserProfileStorage> {
        match self {
            Self::Deferred => None,
            Self::Persistent(storage) => Some(storage),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeron_proto::{AuthState, WorkspaceScope};

    fn scratch_parent() -> PathBuf {
        PathBuf::from("/root/.hermes/profiles/girlfriend/cache/scratch")
    }

    #[test]
    fn rejects_invalid_locators() {
        let dir = scratch_parent();
        assert!(BrowserProfileStorage::from_locator(&dir, "short").is_err());
        assert!(BrowserProfileStorage::from_locator(&dir, "gggggggggggggggg").is_err());
        assert!(BrowserProfileStorage::from_locator(&dir, "../../../etc/passwd").is_err());
    }

    #[test]
    fn storage_paths_are_opaque_and_locator_scoped() {
        let dir = scratch_parent().join("zeron-browser-profile-test");
        let _ = std::fs::create_dir_all(&dir);
        let a = BrowserProfileStorage::from_locator(&dir, "abc123def4567890").unwrap();
        let b = BrowserProfileStorage::from_locator(&dir, "fedcba0987654321").unwrap();
        assert!(a.storage_root().starts_with(&dir));
        assert!(a.storage_root().ends_with("abc123def4567890"));
        assert_ne!(a.storage_root(), b.storage_root());
        assert!(!a.storage_root().to_string_lossy().contains("user:"));
        assert!(!a.storage_root().to_string_lossy().contains("device:"));
    }

    #[test]
    fn distinct_workspace_locators_get_distinct_store_ids() {
        let dir = scratch_parent().join("zeron-browser-profile-test");
        let _ = std::fs::create_dir_all(&dir);
        let scope = Some(WorkspaceScope::Synced);
        let auth_a = AuthState::SignedIn {
            user: zeron_proto::UserProfile {
                id: "user-a".into(),
                email: "a@example.com".into(),
                name: None,
            },
            org_id: None,
        };
        let auth_b = AuthState::SignedIn {
            user: zeron_proto::UserProfile {
                id: "user-b".into(),
                email: "b@example.com".into(),
                name: None,
            },
            org_id: None,
        };
        let loc_a = crate::links::workspace_locator(scope, Some(&auth_a), Some("device")).unwrap();
        let loc_b = crate::links::workspace_locator(scope, Some(&auth_b), Some("device")).unwrap();
        let store_a = BrowserProfileStorage::from_locator(&dir, &loc_a)
            .unwrap()
            .store_uuid();
        let store_b = BrowserProfileStorage::from_locator(&dir, &loc_b)
            .unwrap()
            .store_uuid();
        assert_ne!(loc_a, loc_b);
        assert_ne!(store_a, store_b);
    }

    #[test]
    fn store_uuid_is_stable_for_one_locator() {
        let dir = scratch_parent().join("zeron-browser-profile-test");
        let _ = std::fs::create_dir_all(&dir);
        let storage = BrowserProfileStorage::from_locator(&dir, "0123456789abcdef").unwrap();
        assert_eq!(storage.store_uuid(), storage.store_uuid());
    }
}
