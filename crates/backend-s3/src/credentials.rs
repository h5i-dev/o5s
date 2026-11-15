//! Credential sources beyond static keys: the default provider chain.
//!
//! [`default_chain`] picks the first source that applies, like the AWS SDKs:
//! 1. `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` (`AWS_SESSION_TOKEN`): static.
//! 2. `AWS_WEB_IDENTITY_TOKEN_FILE` + `AWS_ROLE_ARN` (EKS IRSA, GitHub OIDC, …):
//!    STS `AssumeRoleWithWebIdentity`, re-reading the token file on each refresh.
//! 3. EC2 instance metadata (IMDSv2 only; session token required).
//!
//! Temporary credentials live in a [`RefreshingCredentials`] cache. A
//! background task refreshes them [`REFRESH_BEFORE`] ahead of expiry, retrying
//! with backoff on failure, and `credentials()` never blocks. Requests fail
//! only if the credentials actually expire.
//!
//! Endpoint overrides (also used by tests): `AWS_EC2_METADATA_SERVICE_ENDPOINT`
//! and `AWS_ENDPOINT_URL_STS`.

use crate::config::{Credentials, CredentialsProvider};
use crate::error::{S3Error, S3Result};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

/// Refresh this long before expiry.
pub const REFRESH_BEFORE: Duration = Duration::from_secs(5 * 60);
const IMDS_DEFAULT: &str = "http://169.254.169.254";

/// Where temporary credentials come from.
#[derive(Clone, Debug)]
pub enum Source {
    Imds { endpoint: String },
    WebIdentity { token_file: std::path::PathBuf, role_arn: String, session_name: String, sts_endpoint: String },
}

impl Source {
    fn describe(&self) -> String {
        match self {
            Source::Imds { endpoint } => format!("imds({endpoint})"),
            Source::WebIdentity { role_arn, .. } => format!("web-identity({role_arn})"),
        }
    }
}

/// Temporary credentials and their expiry.
#[derive(Clone, Debug)]
pub struct Temporary {
    pub creds: Credentials,
    pub expires: Option<SystemTime>,
}

/// Parses `YYYY-MM-DDTHH:MM:SS[.fff]Z` (what IMDS and STS return).
pub fn parse_rfc3339(s: &str) -> Option<SystemTime> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || !s.ends_with('Z')
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    // days from civil (Howard Hinnant)
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + mi * 60 + se;
    u64::try_from(secs).ok().map(|s| UNIX_EPOCH + Duration::from_secs(s))
}

fn http() -> S3Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| S3Error::Config(format!("http client: {e}")))
}

async fn text(resp: reqwest::Response, what: &str) -> S3Result<String> {
    let status = resp.status();
    let body = resp.text().await.map_err(|e| S3Error::Transport(format!("{what}: {e}")))?;
    if !status.is_success() {
        let snippet: String = body.chars().take(200).collect();
        return Err(S3Error::Config(format!("{what}: HTTP {status}: {snippet}")));
    }
    Ok(body)
}

/// IMDSv2: session token, role name, then role credentials.
async fn fetch_imds(endpoint: &str) -> S3Result<Temporary> {
    let c = http()?;
    let base = endpoint.trim_end_matches('/');
    let tok = c
        .put(format!("{base}/latest/api/token"))
        .header("x-aws-ec2-metadata-token-ttl-seconds", "21600")
        .send()
        .await
        .map_err(|e| S3Error::Transport(format!("IMDS token: {e}")))?;
    let tok = text(tok, "IMDS token").await?;
    let get = |path: String| {
        let c = c.clone();
        let tok = tok.clone();
        async move {
            let r = c
                .get(format!("{base}{path}"))
                .header("x-aws-ec2-metadata-token", tok)
                .send()
                .await
                .map_err(|e| S3Error::Transport(format!("IMDS {path}: {e}")))?;
            text(r, &format!("IMDS {path}")).await
        }
    };
    let roles = get("/latest/meta-data/iam/security-credentials/".into()).await?;
    let role = roles.lines().next().map(str::trim).filter(|r| !r.is_empty() && !r.contains('/') && !r.contains('<'));
    let role = role.ok_or_else(|| S3Error::Config("IMDS: no instance profile role attached".into()))?;
    let body = get(format!("/latest/meta-data/iam/security-credentials/{role}")).await?;
    let v: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| S3Error::Protocol(format!("IMDS credentials JSON: {e}")))?;
    let field = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    if field("Code").is_some_and(|c| c != "Success") {
        return Err(S3Error::Config(format!("IMDS credentials: {}", field("Message").unwrap_or_default())));
    }
    let creds = Credentials {
        access_key_id: field("AccessKeyId").ok_or_else(|| S3Error::Protocol("IMDS: no AccessKeyId".into()))?,
        secret_access_key: field("SecretAccessKey")
            .ok_or_else(|| S3Error::Protocol("IMDS: no SecretAccessKey".into()))?,
        session_token: field("Token"),
    };
    Ok(Temporary { creds, expires: field("Expiration").as_deref().and_then(parse_rfc3339) })
}

/// STS AssumeRoleWithWebIdentity (unsigned; the token is the credential).
async fn fetch_web_identity(
    token_file: &std::path::Path,
    role_arn: &str,
    session: &str,
    sts: &str,
) -> S3Result<Temporary> {
    let token = tokio::fs::read_to_string(token_file).await?;
    let c = http()?;
    let form = [
        ("Action", "AssumeRoleWithWebIdentity"),
        ("Version", "2011-06-15"),
        ("RoleArn", role_arn),
        ("RoleSessionName", session),
        ("WebIdentityToken", token.trim()),
    ];
    let body =
        form.iter().map(|(k, v)| format!("{k}={}", crate::sigv4::uri_encode(v, false))).collect::<Vec<_>>().join("&");
    let r = c
        .post(sts)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "application/xml")
        .body(body)
        .send()
        .await
        .map_err(|e| S3Error::Transport(format!("STS: {e}")))?;
    let xml = text(r, "STS AssumeRoleWithWebIdentity").await?;
    let get = |tag: &str| crate::xml::find_text(xml.as_bytes(), tag);
    let creds = Credentials {
        access_key_id: get("AccessKeyId").ok_or_else(|| S3Error::Protocol("STS: no AccessKeyId".into()))?,
        secret_access_key: get("SecretAccessKey").ok_or_else(|| S3Error::Protocol("STS: no SecretAccessKey".into()))?,
        session_token: get("SessionToken"),
    };
    Ok(Temporary { creds, expires: get("Expiration").as_deref().and_then(parse_rfc3339) })
}

pub async fn fetch(source: &Source) -> S3Result<Temporary> {
    match source {
        Source::Imds { endpoint } => fetch_imds(endpoint).await,
        Source::WebIdentity { token_file, role_arn, session_name, sts_endpoint } => {
            fetch_web_identity(token_file, role_arn, session_name, sts_endpoint).await
        }
    }
}

/// A cache of temporary credentials refreshed in the background.
pub struct RefreshingCredentials {
    source: Source,
    current: RwLock<Temporary>,
}

impl std::fmt::Debug for RefreshingCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefreshingCredentials").field("source", &self.source.describe()).finish()
    }
}

impl RefreshingCredentials {
    /// Fetches once (fails if the source is unusable), then spawns the refresh
    /// task on `rt`. The task stops when the cache is dropped.
    pub async fn start(source: Source, rt: &tokio::runtime::Handle) -> S3Result<Arc<RefreshingCredentials>> {
        let first = fetch(&source).await?;
        info!(source = %source.describe(), expires = ?first.expires, "obtained temporary credentials");
        let me = Arc::new(RefreshingCredentials { source, current: RwLock::new(first) });
        let weak = Arc::downgrade(&me);
        rt.spawn(async move {
            let mut failures = 0u32;
            loop {
                let Some(me) = weak.upgrade() else { return };
                let expires = me.current.read().ok().and_then(|g| g.expires);
                let wait = match expires {
                    // Refresh REFRESH_BEFORE ahead (at least 1 s from now).
                    Some(e) => e
                        .checked_sub(REFRESH_BEFORE)
                        .and_then(|t| t.duration_since(SystemTime::now()).ok())
                        .unwrap_or(Duration::from_secs(1))
                        .max(Duration::from_secs(1)),
                    None => Duration::from_secs(3600),
                };
                let wait = if failures > 0 { Duration::from_secs(1 << failures.min(6)).min(wait) } else { wait };
                let source = me.source.clone();
                drop(me);
                tokio::time::sleep(wait).await;
                let Some(me) = weak.upgrade() else { return };
                match fetch(&source).await {
                    Ok(t) => {
                        failures = 0;
                        info!(source = %source.describe(), expires = ?t.expires, "refreshed temporary credentials");
                        if let Ok(mut g) = me.current.write() {
                            *g = t;
                        }
                    }
                    Err(e) => {
                        failures += 1;
                        warn!(source = %source.describe(), error = %e, failures, "credential refresh failed; will retry");
                    }
                }
            }
        });
        Ok(me)
    }

    pub fn expires(&self) -> Option<SystemTime> {
        self.current.read().ok().and_then(|g| g.expires)
    }
}

impl CredentialsProvider for RefreshingCredentials {
    fn credentials(&self) -> S3Result<Credentials> {
        let g = self.current.read().map_err(|_| S3Error::Config("credential cache poisoned".into()))?;
        if g.expires.is_some_and(|e| e <= SystemTime::now()) {
            return Err(S3Error::Config(format!(
                "{} credentials expired and could not be refreshed",
                self.source.describe()
            )));
        }
        Ok(g.creds.clone())
    }
}

/// Resolves the default chain (see the module docs) on runtime `rt`.
pub async fn default_chain(region: &str, rt: &tokio::runtime::Handle) -> S3Result<Arc<dyn CredentialsProvider>> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    if env("AWS_ACCESS_KEY_ID").is_some() {
        info!("using static credentials from the environment");
        return Ok(Arc::new(Credentials::from_env()?));
    }
    if let (Some(token_file), Some(role_arn)) = (env("AWS_WEB_IDENTITY_TOKEN_FILE"), env("AWS_ROLE_ARN")) {
        let sts_endpoint =
            env("AWS_ENDPOINT_URL_STS").unwrap_or_else(|| format!("https://sts.{region}.amazonaws.com/"));
        let session_name = env("AWS_ROLE_SESSION_NAME").unwrap_or_else(|| "pico2peta".into());
        let src = Source::WebIdentity { token_file: token_file.into(), role_arn, session_name, sts_endpoint };
        return Ok(RefreshingCredentials::start(src, rt).await?);
    }
    let endpoint = env("AWS_EC2_METADATA_SERVICE_ENDPOINT").unwrap_or_else(|| IMDS_DEFAULT.into());
    match RefreshingCredentials::start(Source::Imds { endpoint }, rt).await {
        Ok(p) => Ok(p),
        Err(e) => Err(S3Error::Config(format!("no AWS credentials found (env, web identity, IMDS): {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339() {
        assert_eq!(parse_rfc3339("2013-05-24T00:00:00Z"), Some(UNIX_EPOCH + Duration::from_secs(1_369_353_600)));
        assert_eq!(parse_rfc3339("2000-02-29T12:34:56.789Z"), Some(UNIX_EPOCH + Duration::from_secs(951_827_696)));
        for bad in ["", "2013-05-24", "2013-13-24T00:00:00Z", "2013-05-24T00:00:00+01:00", "abcd-ef-ghTij:kl:mnZ"] {
            assert_eq!(parse_rfc3339(bad), None, "{bad}");
        }
    }
}
