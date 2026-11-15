//! AWS Signature Version 4 request signing (header-based, service `s3`).
//!
//! Reference: <https://docs.aws.amazon.com/AmazonS3/latest/API/sig-v4-header-based-auth.html>.
//! The unit tests reproduce the worked examples from that page.

use crate::config::Credentials;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

/// SHA-256 of the empty string, the payload hash of body-less requests.
pub const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

pub fn hex(bytes: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 15) as usize] as char);
    }
    s
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hmac(key: &[u8], msg: &[u8]) -> [u8; 32] {
    // HMAC accepts keys of any length; `new_from_slice` cannot fail for HMAC.
    let mut m = match HmacSha256::new_from_slice(key) {
        Ok(m) => m,
        Err(_) => unreachable!("HMAC accepts any key length"),
    };
    m.update(msg);
    m.finalize().into_bytes().into()
}

/// URI-encodes per SigV4 rules: unreserved characters pass through, everything else
/// becomes `%XX` (uppercase). `/` is kept iff `keep_slash`.
pub fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') || (keep_slash && b == b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `(YYYYMMDD'T'HHMMSS'Z', YYYYMMDD)` in UTC.
pub fn amz_dates(t: SystemTime) -> (String, String) {
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    let date = format!("{y:04}{m:02}{d:02}");
    let full = format!("{date}T{:02}{:02}{:02}Z", rem / 3600, (rem / 60) % 60, rem % 60);
    (full, date)
}

/// Howard Hinnant's days-from-civil inverse (proleptic Gregorian).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// A request to be signed.
pub struct SignInput<'a> {
    pub method: &'a str,
    /// Already-encoded canonical URI (e.g. `/bucket/key%20x`).
    pub canonical_uri: &'a str,
    /// Raw (unencoded) query parameters.
    pub query: &'a [(String, String)],
    /// `host[:port]` exactly as sent in the `Host` header.
    pub host: &'a str,
    /// Extra headers to sign, names in any case.
    pub headers: &'a [(String, String)],
    /// Hex SHA-256 of the body, or `UNSIGNED-PAYLOAD`.
    pub payload_sha256: &'a str,
    pub region: &'a str,
    pub time: SystemTime,
}

/// The canonical query string (sorted, encoded).
pub fn canonical_query(q: &[(String, String)]) -> String {
    let mut enc: Vec<(String, String)> = q.iter().map(|(k, v)| (uri_encode(k, false), uri_encode(v, false))).collect();
    enc.sort();
    enc.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&")
}

/// Signs `inp`. Returns every header the request must carry in addition to
/// `inp.headers`: `x-amz-date`, `x-amz-content-sha256`, optional
/// `x-amz-security-token`, and `authorization`.
pub fn sign(creds: &Credentials, inp: &SignInput<'_>) -> Vec<(String, String)> {
    let (amz_date, date) = amz_dates(inp.time);
    let mut extra = vec![
        ("x-amz-content-sha256".to_string(), inp.payload_sha256.to_string()),
        ("x-amz-date".to_string(), amz_date.clone()),
    ];
    if let Some(tok) = &creds.session_token {
        extra.push(("x-amz-security-token".to_string(), tok.clone()));
    }

    let mut hdrs: Vec<(String, String)> = Vec::with_capacity(inp.headers.len() + 4);
    hdrs.push(("host".to_string(), inp.host.to_string()));
    for (k, v) in inp.headers.iter().chain(extra.iter()) {
        hdrs.push((k.to_ascii_lowercase(), v.split_whitespace().collect::<Vec<_>>().join(" ")));
    }
    hdrs.sort();
    let canonical_headers: String = hdrs.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = hdrs.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");

    let creq = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        inp.method,
        inp.canonical_uri,
        canonical_query(inp.query),
        canonical_headers,
        signed_headers,
        inp.payload_sha256
    );
    let scope = format!("{date}/{}/s3/aws4_request", inp.region);
    let sts = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", sha256_hex(creq.as_bytes()));

    let k_date = hmac(format!("AWS4{}", creds.secret_access_key).as_bytes(), date.as_bytes());
    let k_region = hmac(&k_date, inp.region.as_bytes());
    let k_service = hmac(&k_region, b"s3");
    let k_signing = hmac(&k_service, b"aws4_request");
    let sig = hex(&hmac(&k_signing, sts.as_bytes()));

    extra.push((
        "authorization".to_string(),
        format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope},SignedHeaders={signed_headers},Signature={sig}",
            creds.access_key_id
        ),
    ));
    extra
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn example_creds() -> Credentials {
        Credentials::new("AKIAIOSFODNN7EXAMPLE", "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY")
    }

    fn t_20130524() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_369_353_600) // 2013-05-24T00:00:00Z
    }

    fn sig_of(h: &[(String, String)]) -> String {
        let auth = &h.iter().find(|(k, _)| k == "authorization").unwrap().1;
        auth.rsplit("Signature=").next().unwrap().to_string()
    }

    #[test]
    fn dates() {
        assert_eq!(amz_dates(t_20130524()), ("20130524T000000Z".into(), "20130524".into()));
        assert_eq!(amz_dates(UNIX_EPOCH + Duration::from_secs(951_782_400)).1, "20000229");
        assert_eq!(amz_dates(UNIX_EPOCH + Duration::from_secs(4_102_444_799)).0, "20991231T235959Z");
    }

    #[test]
    fn aws_example_get_object() {
        let h = sign(
            &example_creds(),
            &SignInput {
                method: "GET",
                canonical_uri: "/test.txt",
                query: &[],
                host: "examplebucket.s3.amazonaws.com",
                headers: &[("Range".into(), "bytes=0-9".into())],
                payload_sha256: EMPTY_SHA256,
                region: "us-east-1",
                time: t_20130524(),
            },
        );
        assert_eq!(sig_of(&h), "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41");
    }

    #[test]
    fn aws_example_put_object() {
        let body = b"Welcome to Amazon S3.";
        let h = sign(
            &example_creds(),
            &SignInput {
                method: "PUT",
                canonical_uri: &format!("/{}", uri_encode("test$file.text", true)),
                query: &[],
                host: "examplebucket.s3.amazonaws.com",
                headers: &[
                    ("Date".into(), "Fri, 24 May 2013 00:00:00 GMT".into()),
                    ("x-amz-storage-class".into(), "REDUCED_REDUNDANCY".into()),
                ],
                payload_sha256: &sha256_hex(body),
                region: "us-east-1",
                time: t_20130524(),
            },
        );
        assert_eq!(sig_of(&h), "98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd");
    }

    #[test]
    fn aws_example_list_objects() {
        let h = sign(
            &example_creds(),
            &SignInput {
                method: "GET",
                canonical_uri: "/",
                query: &[("max-keys".into(), "2".into()), ("prefix".into(), "J".into())],
                host: "examplebucket.s3.amazonaws.com",
                headers: &[],
                payload_sha256: EMPTY_SHA256,
                region: "us-east-1",
                time: t_20130524(),
            },
        );
        assert_eq!(sig_of(&h), "34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7");
    }

    #[test]
    fn encoding() {
        assert_eq!(uri_encode("a b/c~d*", true), "a%20b/c~d%2A");
        assert_eq!(uri_encode("a/b", false), "a%2Fb");
        assert_eq!(canonical_query(&[("uploads".into(), String::new())]), "uploads=");
    }
}
