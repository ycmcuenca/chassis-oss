//! Config directory resolution and the stable host id.

use std::fs;
use std::path::{Path, PathBuf};

use rand::Rng;

use crate::cli::Error;

/// Environment variable overriding the config directory.
pub const CONFIG_DIR_ENV: &str = "CHASSIS_DAEMON_CONFIG_DIR";

const HOST_ID_FILE: &str = "host-id";

/// Resolve the config directory: the env override if set and non-empty, else the platform default.
pub fn resolve_dir() -> Result<PathBuf, Error> {
    match std::env::var_os(CONFIG_DIR_ENV) {
        Some(value) if !value.is_empty() => Ok(PathBuf::from(value)),
        _ => directories::ProjectDirs::from("dev", "ycmcuenca", "chassis-daemon")
            .map(|dirs| dirs.config_dir().to_path_buf())
            .ok_or_else(|| Error::new("could not determine a config directory for this user")),
    }
}

/// Create the config directory (and parents), tightening it to 0700 on Unix.
pub fn ensure_dir(dir: &Path) -> Result<(), Error> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|e| Error::new(format!("cannot create {}: {e}", dir.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|e| Error::new(format!("cannot set permissions on {}: {e}", dir.display())))?;
    }
    Ok(())
}

/// Return the stable host id stored in `<dir>/host-id`, generating it on first use.
pub fn host_id(dir: &Path) -> Result<String, Error> {
    ensure_dir(dir)?;
    let path = dir.join(HOST_ID_FILE);
    if let Ok(existing) = fs::read_to_string(&path) {
        let existing = existing.trim();
        if is_well_formed(existing) {
            return Ok(existing.to_owned());
        }
    }
    let id = generate_uuid_v4();
    fs::write(&path, format!("{id}\n"))
        .map_err(|e| Error::new(format!("cannot write {}: {e}", path.display())))?;
    Ok(id)
}

fn generate_uuid_v4() -> String {
    let mut b = [0u8; 16];
    rand::rng().fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex::encode(b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

fn is_well_formed(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    let hex_lower = |c: u8| c.is_ascii_digit() || (b'a'..=b'f').contains(&c);
    for (i, &c) in bytes.iter().enumerate() {
        let ok = match i {
            8 | 13 | 18 | 23 => c == b'-',
            14 => c == b'4',
            19 => matches!(c, b'8' | b'9' | b'a' | b'b'),
            _ => hex_lower(c),
        };
        if !ok {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_id_is_stable_and_well_formed() {
        let dir = tempfile::tempdir().unwrap();
        let first = host_id(dir.path()).unwrap();
        let second = host_id(dir.path()).unwrap();
        assert_eq!(first, second);
        assert!(is_well_formed(&first), "bad format: {first}");
    }

    #[test]
    fn malformed_host_id_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(HOST_ID_FILE), "nonsense\n").unwrap();
        let id = host_id(dir.path()).unwrap();
        assert!(is_well_formed(&id));
        assert_eq!(host_id(dir.path()).unwrap(), id);
    }

    #[test]
    fn ensure_dir_creates_nested_directory() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b");
        ensure_dir(&nested).unwrap();
        assert!(nested.is_dir());
    }
}
