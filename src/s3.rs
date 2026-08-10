use std::env;
use std::fmt;
use std::net::IpAddr;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use sha2::{Digest, Sha256};
use url::{Position, Url};

type HmacSha256 = Hmac<Sha256>;

const AWS_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'!')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

#[derive(Clone, PartialEq, Eq)]
pub struct S3Config {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: String,
    pub path_style: bool,
}

impl fmt::Debug for S3Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3Config")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("access_key", &self.access_key)
            .field("secret_key", &"[REDACTED]")
            .field("path_style", &self.path_style)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Error(String);

impl fmt::Display for S3Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for S3Error {}

impl S3Config {
    pub fn from_env() -> Result<Self, S3Error> {
        fn required(name: &str) -> Result<String, S3Error> {
            env::var(name)
                .ok()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| S3Error(format!("{name} is required when STORAGE_BACKEND=s3")))
        }

        let path_style = match env::var("S3_PATH_STYLE").as_deref() {
            Ok("1" | "true") => true,
            Ok("0" | "false") | Err(_) => false,
            Ok(value) => {
                return Err(S3Error(format!(
                    "S3_PATH_STYLE must be `true`, `false`, `1`, or `0`, got `{value}`"
                )));
            }
        };
        let config = Self {
            endpoint: required("S3_ENDPOINT")?,
            region: required("S3_REGION")?,
            bucket: required("S3_BUCKET")?,
            access_key: required("S3_ACCESS_KEY")?,
            secret_key: required("S3_SECRET_KEY")?,
            path_style,
        };
        config.endpoint()?;
        Ok(config)
    }

    pub fn presign_get(
        &self,
        key: &str,
        expires: Duration,
        time: SystemTime,
    ) -> Result<String, S3Error> {
        self.presign("GET", key, expires, time)
    }

    pub fn presign_put(
        &self,
        key: &str,
        expires: Duration,
        time: SystemTime,
    ) -> Result<String, S3Error> {
        self.presign("PUT", key, expires, time)
    }

    pub fn presign_get_now(&self, key: &str, expires: Duration) -> Result<String, S3Error> {
        self.presign_get(key, expires, SystemTime::now())
    }

    pub fn presign_put_now(&self, key: &str, expires: Duration) -> Result<String, S3Error> {
        self.presign_put(key, expires, SystemTime::now())
    }

    fn presign(
        &self,
        method: &str,
        key: &str,
        expires: Duration,
        time: SystemTime,
    ) -> Result<String, S3Error> {
        let expires = expires.as_secs();
        if !(1..=604_800).contains(&expires) {
            return Err(S3Error(
                "S3 presign expiry must be 1..=604800 seconds".into(),
            ));
        }
        if key.is_empty() || key.starts_with('/') {
            return Err(S3Error(
                "S3 object key must be non-empty and relative".into(),
            ));
        }

        let mut endpoint = self.endpoint()?;
        let virtual_hosted = !self.path_style && virtual_host_is_safe(&endpoint, &self.bucket);
        if virtual_hosted {
            let host = format!("{}.{}", self.bucket, endpoint.host_str().unwrap());
            endpoint
                .set_host(Some(&host))
                .map_err(|_| S3Error("S3 virtual-hosted endpoint is invalid".into()))?;
        }
        let host = endpoint[Position::BeforeHost..Position::AfterPort].to_string();
        let base_path = endpoint.path().trim_end_matches('/');
        let object = aws_encode(key, false);
        let canonical_uri = if virtual_hosted {
            format!("{base_path}/{object}")
        } else {
            format!("{base_path}/{}/{object}", aws_encode(&self.bucket, true))
        };

        let time: DateTime<Utc> = time.into();
        let amz_date = time.format("%Y%m%dT%H%M%SZ").to_string();
        let date = time.format("%Y%m%d").to_string();
        let scope = format!("{date}/{}/s3/aws4_request", self.region);
        let credential = aws_encode(&format!("{}/{scope}", self.access_key), true);
        let query = format!(
            "X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential={credential}&X-Amz-Date={amz_date}&X-Amz-Expires={expires}&X-Amz-SignedHeaders=host"
        );
        let canonical_request =
            format!("{method}\n{canonical_uri}\n{query}\nhost:{host}\n\nhost\nUNSIGNED-PAYLOAD");
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            hex(&Sha256::digest(canonical_request.as_bytes()))
        );
        let date_key = hmac(
            format!("AWS4{}", self.secret_key).as_bytes(),
            date.as_bytes(),
        );
        let region_key = hmac(&date_key, self.region.as_bytes());
        let service_key = hmac(&region_key, b"s3");
        let signing_key = hmac(&service_key, b"aws4_request");
        let signature = hex(&hmac(&signing_key, string_to_sign.as_bytes()));

        let origin = &endpoint[..Position::BeforePath];
        Ok(format!(
            "{origin}{canonical_uri}?{query}&X-Amz-Signature={signature}"
        ))
    }

    fn endpoint(&self) -> Result<Url, S3Error> {
        let endpoint = Url::parse(&self.endpoint)
            .map_err(|error| S3Error(format!("invalid S3_ENDPOINT: {error}")))?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(S3Error(
                "S3_ENDPOINT must be an HTTP(S) URL without credentials, query, or fragment".into(),
            ));
        }
        Ok(endpoint)
    }
}

fn aws_encode(value: &str, encode_slash: bool) -> String {
    let encoded = utf8_percent_encode(value, AWS_ENCODE_SET).to_string();
    if encode_slash {
        encoded.replace('/', "%2F")
    } else {
        encoded
    }
}

fn hmac(key: &[u8], value: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(value);
    mac.finalize().into_bytes().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    use fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            write!(out, "{byte:02x}").expect("writing to String cannot fail");
            out
        })
}

fn virtual_host_is_safe(endpoint: &Url, bucket: &str) -> bool {
    let host = endpoint.host_str().unwrap_or_default();
    let valid_bucket = (3..=63).contains(&bucket.len())
        && !bucket.contains("..")
        && bucket.parse::<IpAddr>().is_err()
        && bucket.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        });
    valid_bucket
        && host.parse::<IpAddr>().is_err()
        && host.contains('.')
        && !(endpoint.scheme() == "https" && bucket.contains('.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn aws_example() -> S3Config {
        S3Config {
            endpoint: "https://s3.amazonaws.com".into(),
            region: "us-east-1".into(),
            bucket: "examplebucket".into(),
            access_key: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            path_style: false,
        }
    }

    #[test]
    fn matches_aws_s3_presigned_url_example() {
        let time = UNIX_EPOCH + Duration::from_secs(1_369_353_600); // 2013-05-24T00:00:00Z
        let url = aws_example()
            .presign_get("test.txt", Duration::from_secs(86_400), time)
            .unwrap();
        assert_eq!(
            url,
            "https://examplebucket.s3.amazonaws.com/test.txt?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request&X-Amz-Date=20130524T000000Z&X-Amz-Expires=86400&X-Amz-SignedHeaders=host&X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404"
        );
    }

    #[test]
    fn put_path_style_encodes_key_without_normalizing_it() {
        let mut config = aws_example();
        config.endpoint = "http://127.0.0.1:9000/api".into();
        config.path_style = false;
        let url = config
            .presign_put(
                "folder/a b+%E2%98%83",
                Duration::from_secs(60),
                UNIX_EPOCH + Duration::from_secs(1_369_353_600),
            )
            .unwrap();
        assert!(url.starts_with(
            "http://127.0.0.1:9000/api/examplebucket/folder/a%20b%2B%25E2%2598%2583?"
        ));
        assert!(url.contains("X-Amz-Expires=60"));
    }

    #[test]
    fn dotted_https_bucket_falls_back_to_path_style() {
        let mut config = aws_example();
        config.bucket = "recordings.example".into();
        let url = config
            .presign_get(
                "video.mp4",
                Duration::from_secs(1),
                UNIX_EPOCH + Duration::from_secs(1_369_353_600),
            )
            .unwrap();
        assert!(url.starts_with("https://s3.amazonaws.com/recordings.example/video.mp4?"));
    }

    #[test]
    fn localhost_falls_back_to_path_style() {
        let mut config = aws_example();
        config.endpoint = "http://localhost:9000".into();
        let url = config
            .presign_get(
                "video.mp4",
                Duration::from_secs(1),
                UNIX_EPOCH + Duration::from_secs(1_369_353_600),
            )
            .unwrap();
        assert!(url.starts_with("http://localhost:9000/examplebucket/video.mp4?"));
    }

    #[test]
    fn rejects_invalid_expiry_and_endpoint() {
        let config = aws_example();
        assert!(config.presign_get("x", Duration::ZERO, UNIX_EPOCH).is_err());
        assert!(
            config
                .presign_get("x", Duration::from_secs(604_801), UNIX_EPOCH)
                .is_err()
        );
        let mut config = config;
        config.endpoint = "https://user@example.com?x=1".into();
        assert!(
            config
                .presign_get("x", Duration::from_secs(1), UNIX_EPOCH)
                .is_err()
        );
    }
}
