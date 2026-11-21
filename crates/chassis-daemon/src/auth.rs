//! Pairing token issue and verification.

use std::fs;
use std::io::Write;
use std::path::Path;

use base64::Engine;
use rand::Rng;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::cli::Error;
use crate::config;

/// File (inside the config dir) holding the hex SHA-256 of the current token.
pub const TOKEN_HASH_FILE: &str = "token.sha256";

/// The stored hash of the current pairing token.
#[derive(Debug, Clone)]
pub struct TokenHash(String);

impl TokenHash {
    /// Load `<dir>/token.sha256`. `None` when the file is missing; error when malformed.
    pub fn load(dir: &Path) -> Result<Option<TokenHash>, Error> {
        let path = dir.join(TOKEN_HASH_FILE);
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::new(format!("cannot read {}: {e}", path.display()))),
        };
        let hash = raw.trim();
        let well_formed = hash.len() == 64
            && hash
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c));
        if !well_formed {
            return Err(Error::new(format!(
                "{} is malformed; run `chassis-daemon pair` again",
                path.display()
            )));
        }
        Ok(Some(TokenHash(hash.to_owned())))
    }

    /// Constant-time check of a candidate token against the stored hash.
    pub fn verify(&self, token: &str) -> bool {
        let candidate = hash_token(token);
        self.0.as_bytes().ct_eq(candidate.as_bytes()).into()
    }
}

fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Issue a new pairing token, replacing (and so invalidating) any previous one.
/// Returns the token; only its hash is stored.
pub fn pair(dir: &Path) -> Result<String, Error> {
    config::ensure_dir(dir)?;

    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let contents = format!("{}\n", hash_token(&token));

    let target = dir.join(TOKEN_HASH_FILE);
    let tmp = dir.join(format!("{TOKEN_HASH_FILE}.tmp"));
    write_private(&tmp, contents.as_bytes())
        .and_then(|()| fs::rename(&tmp, &target))
        .map_err(|e| {
            let _ = fs::remove_file(&tmp);
            Error::new(format!("cannot write {}: {e}", target.display()))
        })?;
    Ok(token)
}

fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    // Remove a stale temp file so the 0600 mode applies to a fresh file.
    let _ = fs::remove_file(path);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_hash_file_loads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(TokenHash::load(dir.path()).unwrap().is_none());
    }

    #[test]
    fn malformed_hash_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(TOKEN_HASH_FILE), "not a hash\n").unwrap();
        assert!(TokenHash::load(dir.path()).is_err());
    }

    #[test]
    fn repairing_invalidates_the_previous_token() {
        let dir = tempfile::tempdir().unwrap();
        let first = pair(dir.path()).unwrap();
        assert!(TokenHash::load(dir.path()).unwrap().unwrap().verify(&first));

        let second = pair(dir.path()).unwrap();
        let hash = TokenHash::load(dir.path()).unwrap().unwrap();
        assert!(!hash.verify(&first));
        assert!(hash.verify(&second));
    }

    #[test]
    fn token_is_32_bytes_base64url() {
        let dir = tempfile::tempdir().unwrap();
        let token = pair(dir.path()).unwrap();
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&token)
            .unwrap();
        assert_eq!(decoded.len(), 32);
        assert_eq!(token.len(), 43);
    }

    #[cfg(unix)]
    #[test]
    fn hash_file_is_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        pair(dir.path()).unwrap();
        let mode = fs::metadata(dir.path().join(TOKEN_HASH_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        // Re-pairing keeps the mode.
        pair(dir.path()).unwrap();
        let mode = fs::metadata(dir.path().join(TOKEN_HASH_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
