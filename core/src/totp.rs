//! RFC 6238 TOTP (SHA-1, 6 digits, 30 s) for credentials of the local source.

use hmac::{Hmac, Mac};

/// Accepts a base32 secret or an `otpauth://` URI.
pub fn parse_secret(input: &str) -> Option<Vec<u8>> {
    let raw = input.trim();
    let secret = if raw.starts_with("otpauth://") {
        url::Url::parse(raw)
            .ok()?
            .query_pairs()
            .find(|(k, _)| k == "secret")
            .map(|(_, v)| v.into_owned())?
    } else {
        raw.to_string()
    };
    let mut bits = 0u64;
    let mut n = 0;
    let mut out = Vec::new();
    for c in secret.chars().filter(|c| !c.is_whitespace() && *c != '=' && *c != '-') {
        let v = match c.to_ascii_uppercase() {
            c @ 'A'..='Z' => c as u64 - 'A' as u64,
            c @ '2'..='7' => c as u64 - '2' as u64 + 26,
            _ => return None,
        };
        bits = (bits << 5) | v;
        n += 5;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
        }
    }
    (!out.is_empty()).then_some(out)
}

pub fn code_at(secret: &[u8], unix: u64) -> (String, u32) {
    let step = unix / 30;
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(secret).expect("any key length");
    mac.update(&step.to_be_bytes());
    let h = mac.finalize().into_bytes();
    let off = (h[19] & 0x0f) as usize;
    let bin = u32::from_be_bytes([h[off] & 0x7f, h[off + 1], h[off + 2], h[off + 3]]);
    (format!("{:06}", bin % 1_000_000), 30 - (unix % 30) as u32)
}

#[cfg(test)]
mod tests {
    #[test]
    fn rfc6238_vector() {
        // RFC 6238 SHA-1 test key "12345678901234567890", T = 59 s → 94287082 (8 digits) → 287082.
        let key = b"12345678901234567890";
        assert_eq!(super::code_at(key, 59).0, "287082");
        assert_eq!(super::parse_secret("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ").unwrap(), key.to_vec());
        assert_eq!(
            super::parse_secret("otpauth://totp/x?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=y").unwrap(),
            key.to_vec()
        );
    }
}
