use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

/// Encode path segments for URLs without treating `/` as a delimiter to escape.
const PATH_SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'+')
    .add(b',')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

#[derive(Clone)]
pub struct Signer {
    secret: Vec<u8>,
}

impl Signer {
    pub fn new(secret: &[u8]) -> Self {
        Signer {
            secret: secret.to_vec(),
        }
    }

    fn hmac(&self, method: &str, path: &str, exp: i64) -> String {
        let mut mac = HmacSha256::new_from_slice(&self.secret).expect("hmac key");
        mac.update(method.as_bytes());
        mac.update(b":");
        mac.update(path.as_bytes());
        mac.update(b":");
        mac.update(exp.to_string().as_bytes());
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    }

    fn encode_path(path: &str) -> String {
        path.split('/')
            .map(|segment| utf8_percent_encode(segment, PATH_SEGMENT).to_string())
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Returns a signed GET URL for a storage path, e.g. `/media/owner/video/result.mp4`.
    pub fn get_url(&self, web_url: &str, path: &str, ttl_secs: i64) -> String {
        let exp = now() + ttl_secs;
        let sig = self.hmac("GET", path, exp);
        let encoded = Self::encode_path(path);
        format!("{web_url}/media/{encoded}?exp={exp}&sig={sig}")
    }

    /// Returns a signed PUT URL used for uploads, e.g. `/up/owner/video/result.mp4`.
    pub fn put_url(&self, web_url: &str, path: &str, ttl_secs: i64) -> String {
        let exp = now() + ttl_secs;
        let sig = self.hmac("PUT", path, exp);
        let encoded = Self::encode_path(path);
        format!("{web_url}/up/{encoded}?exp={exp}&sig={sig}")
    }

    /// Verify a signed request. Returns true if signature matches and not expired.
    pub fn verify(&self, method: &str, path: &str, exp: i64, sig: &str) -> bool {
        if now() > exp {
            return false;
        }
        let expected = self.hmac(method, path, exp);
        constant_time_eq(expected.as_bytes(), sig.as_bytes())
    }
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let s = Signer::new(b"secret");
        let url = s.get_url("http://localhost", "user/vid/result.mp4", 60);
        assert!(url.starts_with("http://localhost/media/user/vid/result.mp4?exp="));
        let (_path, query) = url.split_once('?').unwrap();
        let (exp_str, sig) = query.split_once('&').unwrap();
        let exp_str = exp_str.strip_prefix("exp=").unwrap();
        let sig = sig.strip_prefix("sig=").unwrap();
        let exp: i64 = exp_str.parse().unwrap();
        assert!(s.verify("GET", "user/vid/result.mp4", exp, sig));
        assert!(!s.verify("GET", "user/vid/other.mp4", exp, sig));
        assert!(!s.verify("GET", "user/vid/result.mp4", exp - 1000000, sig));
    }

    #[test]
    fn encodes_reserved_path_characters() {
        let s = Signer::new(b"secret");
        let url = s.get_url("http://localhost", "user/vid/file name?.mp4", 60);
        assert!(url.contains("/media/user/vid/file%20name%3F.mp4?"));
        let query = url.split_once('?').unwrap().1;
        let (exp_str, sig) = query.split_once('&').unwrap();
        let exp: i64 = exp_str.strip_prefix("exp=").unwrap().parse().unwrap();
        let sig = sig.strip_prefix("sig=").unwrap();
        assert!(s.verify("GET", "user/vid/file name?.mp4", exp, sig));
    }
}
