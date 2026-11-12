//! Installation-scoped encryption shared by HTTP configuration and source workers.
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE};
use fernet::Fernet;
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Clone)]
pub struct CredentialCipher(Fernet);
impl std::fmt::Debug for CredentialCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CredentialCipher([redacted])")
    }
}
impl CredentialCipher {
    pub fn new(installation_secret: &str) -> Result<Self> {
        ensure!(
            installation_secret.len() >= 32,
            "installation secret requires at least 32 bytes"
        );
        let key = URL_SAFE.encode(Sha256::digest(
            format!("{installation_secret}:source-credentials:v1").as_bytes(),
        ));
        Ok(Self(
            Fernet::new(&key).context("invalid credential encryption key")?,
        ))
    }
    pub fn encrypt(&self, value: &Value) -> Result<String> {
        Ok(self.0.encrypt(&serde_json::to_vec(value)?))
    }
    pub fn decrypt(&self, encrypted: &str) -> Result<Value> {
        let bytes = self
            .0
            .decrypt(encrypted)
            .map_err(|_| anyhow::anyhow!("source credentials cannot be decrypted"))?;
        serde_json::from_slice(&bytes).context("invalid source credential document")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn installation_scope_and_authenticated_ciphertext() {
        let cipher = super::CredentialCipher::new("synthetic-installation-secret-32bytes").unwrap();
        let value = serde_json::json!({"password":"synthetic-password","refresh_token":"synthetic-refresh"});
        let encrypted = cipher.encrypt(&value).unwrap();
        assert!(!encrypted.contains("synthetic"));
        assert_eq!(cipher.decrypt(&encrypted).unwrap(), value);
        let other =
            super::CredentialCipher::new("other-synthetic-installation-secret-32bytes").unwrap();
        assert!(other.decrypt(&encrypted).is_err());
        let mut changed = encrypted.into_bytes();
        changed[20] = if changed[20] == b'A' { b'B' } else { b'A' };
        assert!(
            cipher
                .decrypt(std::str::from_utf8(&changed).unwrap())
                .is_err()
        );
        assert_eq!(format!("{cipher:?}"), "CredentialCipher([redacted])");
    }
}
