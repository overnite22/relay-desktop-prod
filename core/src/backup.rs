//! Backups. Server: encrypted archive produced by the Relay server. Local:
//! consistent copy of the database file, sealed with a key derived from a
//! backup password (Argon2id + AES-256-GCM, see [`crate::vault`]).

use crate::source::{Error, Result};
use crate::vault::{random_salt, Key, SALT_LEN};

const MAGIC: &[u8] = b"RELAY-LOCAL-BACKUP-1\n";
pub const MIN_PASSWORD: usize = 8;

pub fn seal(db: &[u8], password: &str) -> Result<Vec<u8>> {
    if password.chars().count() < MIN_PASSWORD {
        return Err(Error::Invalid(format!(
            "le mot de passe de sauvegarde doit faire au moins {MIN_PASSWORD} caractères"
        )));
    }
    let salt = random_salt();
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&salt);
    out.extend(Key::derive(password, &salt).seal(MAGIC, db));
    Ok(out)
}

/// Returns the database file contained in a local backup.
pub fn open(data: &[u8], password: &str) -> Result<zeroize::Zeroizing<Vec<u8>>> {
    let rest = data
        .strip_prefix(MAGIC)
        .filter(|r| r.len() > SALT_LEN)
        .ok_or_else(|| Error::Invalid("ce fichier n'est pas une sauvegarde de coffre local Relay".into()))?;
    let (salt, sealed) = rest.split_at(SALT_LEN);
    Key::derive(password, salt)
        .open(MAGIC, sealed)
        .ok_or_else(|| Error::Invalid("mot de passe de sauvegarde incorrect ou fichier altéré".into()))
}

/// True when `data` looks like a local backup (and not a server archive).
pub fn is_local(data: &[u8]) -> bool {
    data.starts_with(MAGIC)
}

#[cfg(test)]
mod tests {
    #[test]
    fn roundtrip() {
        let sealed = super::seal(b"SQLite format 3\0...", "sauvegarde!").unwrap();
        assert!(super::is_local(&sealed));
        assert_eq!(super::open(&sealed, "sauvegarde!").unwrap().as_slice(), b"SQLite format 3\0...");
        assert!(super::open(&sealed, "mauvais mdp").is_err());
        assert!(super::seal(b"x", "court").is_err());
        assert!(super::open(b"autre chose", "sauvegarde!").is_err());
    }
}

/// Standard base64 with padding (for the X-Backup-Password header).
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[test]
fn base64_vectors() {
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"f"), "Zg==");
    assert_eq!(base64(b"fo"), "Zm8=");
    assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    assert_eq!(base64("mdp é".as_bytes()), "bWRwIMOp");
}
