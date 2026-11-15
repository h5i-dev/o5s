//! Server configuration (TOML).
//!
//! ```toml
//! [server]
//! listen = "0.0.0.0:8080"
//! max_upload_bytes = 1099511627776   # 1 TiB
//! max_concurrent_requests = 256
//! request_timeout_secs = 3600        # whole request, including body transfer
//! shutdown_grace_secs = 30
//! trust_forwarded_for = false      # true only behind a proxy that sets X-Forwarded-For
//! # tls = { cert = "/etc/pico2peta/tls.crt", key = "/etc/pico2peta/tls.key" }  # native HTTPS
//! # metrics_listen = "10.0.0.5:9100"   # /metrics only there
//! # rate_limit = { per_sec = 50.0, burst = 200.0 }   # per principal
//!
//! [repo]
//! path = "/var/lib/pico2peta/repo"
//! create = true                      # init the repo if missing
//! refs = "local"                     # or "backend": refs on S3, several servers per repo
//!
//! [backend]                          # optional; default: packs on local disk
//! type = "s3"
//! endpoint = "https://s3.us-east-1.amazonaws.com"
//! region = "us-east-1"
//! bucket = "my-bucket"
//! prefix = "pico2peta"
//! path_style = false
//! # credentials: AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY[/AWS_SESSION_TOKEN], else
//! # AWS_WEB_IDENTITY_TOKEN_FILE + AWS_ROLE_ARN (EKS IRSA), else the EC2 instance profile (IMDSv2)
//!
//! [auth]
//! allow_anonymous = false
//! # [auth.oidc]                     # optional: JWT bearer tokens
//! # issuer = "https://accounts.example.com"
//! # audience = "pico2peta"
//! # principal_claim = "email"
//! [[auth.tokens]]
//! principal = "ci"
//! sha256 = "<hex sha256 of the bearer token>"
//!
//! [[policy]]
//! principal = "ci"                   # a principal, "*" (any authenticated) or "anonymous"
//! namespace = "pkgs"                 # prefix; "" = everything
//! access = "write"                   # read | write | admin
//!
//! [audit]
//! path = "/var/log/pico2peta/audit.jsonl"
//! log_reads = false
//! rotate_bytes = 1073741824        # optional: rotate at 1 GiB, chain continues
//!
//! [gc]
//! interval_secs = 0                  # background GC period; 0 = off
//! grace_secs = 3600                  # >= 600; also the put->replace lease
//! grouped = false                    # opt-in similarity-grouped repack (EXP1 did not pass)
//! repack_all = false                 # regroup fully-live packs too (aggressive)
//!
//! [log]
//! format = "json"                    # json | text
//! filter = "info"                    # RUST_LOG syntax
//! ```

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub server: ServerSection,
    pub repo: RepoSection,
    #[serde(default)]
    pub backend: BackendSection,
    #[serde(default)]
    pub auth: AuthSection,
    #[serde(default)]
    pub policy: Vec<PolicyEntry>,
    pub audit: AuditSection,
    #[serde(default)]
    pub log: LogSection,
    #[serde(default)]
    pub gc: GcSection,
    #[serde(default)]
    pub oci: OciSection,
    #[serde(default)]
    pub git: GitSection,
    #[serde(default)]
    pub hf: HfSection,
    #[serde(default)]
    pub pypi: PypiSection,
    #[serde(default)]
    pub npm: NpmSection,
    #[serde(default)]
    pub maven: MavenSection,
    /// `[[promotion]]`: which destination namespaces accept promotions, and what they require.
    #[serde(default)]
    pub promotion: Vec<PromotionRule>,
}

/// A promotion gate (`POST /v1/promote`): promotions into names under `dst` must satisfy it.
/// Names under no rule's `dst` can't be promoted into.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromotionRule {
    /// Destination name prefix (e.g. `prod` or `oci/prod`). The longest match applies.
    pub dst: String,
    /// Hex Ed25519 public keys that must each have signed an attestation about the object.
    #[serde(default)]
    pub required_keys: Vec<String>,
    /// The required attestations must have this in-toto predicate type.
    #[serde(default)]
    pub predicate_type: Option<String>,
    /// The object must have a provenance record that produced it (e.g. a front's publish).
    #[serde(default)]
    pub require_provenance: bool,
}

/// Maven-compatible repositories (`/maven/<repo>/`). Every repository accepts deploys; one
/// with an upstream also proxies and caches it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MavenSection {
    pub enabled: bool,
    /// `[maven.upstream.<repo>]` (e.g. `url = "https://repo.maven.apache.org/maven2"`)
    pub upstream: std::collections::BTreeMap<String, Upstream>,
}

impl Default for MavenSection {
    fn default() -> Self {
        MavenSection { enabled: true, upstream: Default::default() }
    }
}

/// npm-compatible registries (`/npm/<registry>/`). Every registry accepts publishes; one with
/// an upstream also proxies and caches it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NpmSection {
    pub enabled: bool,
    /// `[npm.upstream.<registry>]` (e.g. `url = "https://registry.npmjs.org"`)
    pub upstream: std::collections::BTreeMap<String, Upstream>,
}

impl Default for NpmSection {
    fn default() -> Self {
        NpmSection { enabled: true, upstream: Default::default() }
    }
}

/// An upstream registry that a package index proxies and caches.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upstream {
    /// Base URL (for PyPI, the simple API root, e.g. `https://pypi.org/simple`).
    pub url: String,
    /// Bearer token sent to the upstream, if it needs one.
    #[serde(default)]
    pub token: Option<String>,
    /// How long a fetched upstream listing is reused before asking again.
    #[serde(default = "default_upstream_ttl")]
    pub ttl_secs: u64,
    /// Other hosts the upstream's listings may point file downloads at (the upstream's own
    /// host is always allowed; `pypi.org` implies `files.pythonhosted.org`). Anything else,
    /// and any redirect elsewhere, is refused: listings are upstream-controlled, so this is
    /// what keeps a bad upstream from steering the server at internal addresses (SSRF).
    #[serde(default)]
    pub file_hosts: Vec<String>,
}

fn default_upstream_ttl() -> u64 {
    600
}

impl Upstream {
    /// Hosts this upstream may make the server fetch from.
    pub fn allowed_hosts(&self) -> Vec<String> {
        let mut out: Vec<String> = self.file_hosts.iter().map(|h| h.to_ascii_lowercase()).collect();
        if let Ok(u) = reqwest::Url::parse(&self.url)
            && let Some(h) = u.host_str()
        {
            let h = h.to_ascii_lowercase();
            if h == "pypi.org" {
                out.push("files.pythonhosted.org".into());
            }
            out.push(h);
        }
        out
    }

    /// May the server fetch `url` on behalf of this upstream? (http/https, allowed host.)
    pub fn allows(&self, url: &str) -> bool {
        let Ok(u) = reqwest::Url::parse(url) else { return false };
        matches!(u.scheme(), "http" | "https")
            && u.host_str().is_some_and(|h| self.allowed_hosts().iter().any(|a| a.eq_ignore_ascii_case(h)))
    }
}

/// PyPI-compatible indexes (`/pypi/<index>/simple/`, upload at `/pypi/<index>/`).
/// Every index accepts uploads; an index with an upstream also proxies and caches it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PypiSection {
    pub enabled: bool,
    /// `[pypi.upstream.<index>]`
    pub upstream: std::collections::BTreeMap<String, Upstream>,
}

impl Default for PypiSection {
    fn default() -> Self {
        PypiSection { enabled: true, upstream: Default::default() }
    }
}

/// Hugging Face Hub-compatible endpoints (`/api/...`, `/<repo>/resolve/...`). Needs `[git]`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HfSection {
    pub enabled: bool,
}

impl Default for HfSection {
    fn default() -> Self {
        HfSection { enabled: true }
    }
}

/// Git smart HTTP endpoint (`/git/<repo>.git/...`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GitSection {
    pub enabled: bool,
    /// Spool directory for incoming pushes (default `<repo.path>.git-spool`).
    pub spool_dir: Option<PathBuf>,
    /// Repository-name prefixes whose repositories use `object-format=sha256` (e.g.
    /// `["team/sha256"]`). Fixed per repository: git learns the format before its first push.
    pub sha256_repos: Vec<String>,
}

impl Default for GitSection {
    fn default() -> Self {
        GitSection { enabled: true, spool_dir: None, sha256_repos: Vec::new() }
    }
}

/// OCI distribution endpoint (`/v2/`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OciSection {
    pub enabled: bool,
    /// Spool directory for chunked uploads (default `<repo.path>/../oci-uploads`).
    /// Emptied at startup, since upload sessions do not survive restarts.
    pub upload_dir: Option<PathBuf>,
    pub max_upload_sessions: usize,
}

impl Default for OciSection {
    fn default() -> Self {
        OciSection { enabled: true, upload_dir: None, max_upload_sessions: 1024 }
    }
}

/// Garbage collection (see `pico2peta_repo::gc` for the protocol).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GcSection {
    /// Run GC every this many seconds; 0 disables the background task
    /// (admins can still trigger `POST /v1/admin/gc`).
    pub interval_secs: u64,
    /// Minimum time between GC phases, and the put→replace lease (>= 600).
    pub grace_secs: u64,
    pub min_garbage_ratio: f64,
    pub small_pack_bytes: u64,
    /// Similarity-grouped repack of git content (Experiment 1 V2 layout).
    /// Off by default: Experiment 1 failed its pre-registered thresholds
    /// (redis 1.336x git --aggressive > 1.30), see docs/EXP1.md.
    pub grouped: bool,
    /// Regroup even fully-live packs (like `git gc --aggressive`). Expensive.
    pub repack_all: bool,
}

impl Default for GcSection {
    fn default() -> Self {
        let d = pico2peta_repo::GcOptions::default();
        GcSection {
            interval_secs: 0,
            grace_secs: d.grace.as_secs(),
            min_garbage_ratio: d.min_garbage_ratio,
            small_pack_bytes: d.small_pack_bytes,
            grouped: false,
            repack_all: false,
        }
    }
}

impl GcSection {
    pub fn options_with(&self, dry_run: bool, repack_all: bool) -> pico2peta_repo::GcOptions {
        pico2peta_repo::GcOptions { repack_all: repack_all || self.repack_all, ..self.options(dry_run) }
    }

    pub fn options(&self, dry_run: bool) -> pico2peta_repo::GcOptions {
        pico2peta_repo::GcOptions {
            grace: std::time::Duration::from_secs(self.grace_secs),
            allow_short_grace: false,
            min_garbage_ratio: self.min_garbage_ratio,
            small_pack_bytes: self.small_pack_bytes,
            dry_run,
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerSection {
    pub listen: SocketAddr,
    pub max_upload_bytes: u64,
    pub max_concurrent_requests: usize,
    pub request_timeout_secs: u64,
    pub shutdown_grace_secs: u64,
    /// Take the client address for audit records from `X-Forwarded-For`.
    /// Enable only behind a reverse proxy that sets it.
    pub trust_forwarded_for: bool,
    /// Serve HTTPS directly (otherwise plain HTTP behind a TLS proxy).
    pub tls: Option<TlsSection>,
    /// Serve `/metrics` (plus `/healthz`, `/readyz`) on this separate address only,
    /// e.g. a private interface for Prometheus. Unset: on the main listener.
    pub metrics_listen: Option<SocketAddr>,
    /// Per-principal request rate limit (anonymous callers are keyed by client address).
    pub rate_limit: Option<RateLimit>,
}

/// Token bucket: `per_sec` sustained, `burst` peak.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimit {
    pub per_sec: f64,
    pub burst: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsSection {
    /// PEM certificate chain (leaf first). Reloaded on SIGHUP.
    pub cert: PathBuf,
    /// PEM private key (PKCS#8, PKCS#1 or SEC1). Reloaded on SIGHUP.
    pub key: PathBuf,
}

impl Default for ServerSection {
    fn default() -> Self {
        ServerSection {
            listen: SocketAddr::from(([127, 0, 0, 1], 8080)),
            max_upload_bytes: 1 << 40,
            max_concurrent_requests: 256,
            request_timeout_secs: 3600,
            shutdown_grace_secs: 30,
            trust_forwarded_for: false,
            tls: None,
            metrics_listen: None,
            rate_limit: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoSection {
    pub path: PathBuf,
    #[serde(default)]
    pub create: bool,
    /// Ref store chosen when the repository is created: `local` (journal under
    /// `path`, one host) or `backend` (create-only log on the pack backend, so
    /// several servers sharing one S3 bucket/prefix can serve the repository).
    #[serde(default)]
    pub refs: pico2peta_repo::RefsStore,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum BackendSection {
    /// Sealed packs under `<repo>/objects`.
    #[default]
    Local,
    S3(S3Section),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3Section {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default = "yes")]
    pub path_style: bool,
    #[serde(default = "yes")]
    pub conditional_writes: bool,
    #[serde(default)]
    pub max_in_flight: Option<usize>,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthSection {
    #[serde(default)]
    pub allow_anonymous: bool,
    #[serde(default)]
    pub tokens: Vec<TokenEntry>,
    /// Optional extra TOML file with `[[tokens]]` entries (kept out of the main config).
    #[serde(default)]
    pub token_file: Option<PathBuf>,
    /// Accept OIDC/JWT bearer tokens from this issuer (in addition to static tokens).
    #[serde(default)]
    pub oidc: Option<OidcSection>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcSection {
    /// Exact `iss` value, e.g. `https://accounts.example.com`.
    pub issuer: String,
    /// Required `aud` value.
    pub audience: String,
    /// JWKS location; default: discovered from `<issuer>/.well-known/openid-configuration`.
    #[serde(default)]
    pub jwks_url: Option<String>,
    /// Claim used as the principal name (default `sub`; `email` is common).
    #[serde(default = "default_claim")]
    pub principal_claim: String,
    /// Prefix added to the claim, e.g. `oidc:` (keeps namespaces apart from static principals).
    #[serde(default)]
    pub principal_prefix: String,
    /// Minimum seconds between JWKS refetches triggered by unknown key ids.
    #[serde(default = "default_refetch")]
    pub min_refetch_secs: u64,
}

fn default_refetch() -> u64 {
    30
}

fn default_claim() -> String {
    "sub".into()
}

/// A bearer token, stored only as its SHA-256.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenEntry {
    pub principal: String,
    pub sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    Read,
    Write,
    Admin,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyEntry {
    pub principal: String,
    pub namespace: String,
    pub access: Access,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditSection {
    pub path: PathBuf,
    #[serde(default)]
    pub log_reads: bool,
    /// Rotate the log at this size (renamed to `<path>.<last seq>`; the hash
    /// chain continues). Unset: never rotate.
    #[serde(default)]
    pub rotate_bytes: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LogSection {
    pub format: LogFormat,
    pub filter: String,
}

impl Default for LogSection {
    fn default() -> Self {
        LogSection { format: LogFormat::Json, filter: "info".into() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Json,
    Text,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading {0}: {1}")]
    Read(PathBuf, std::io::Error),
    #[error("parsing {0}: {1}")]
    Parse(PathBuf, String),
    #[error("invalid config: {0}")]
    Invalid(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenFile {
    #[serde(default)]
    tokens: Vec<TokenEntry>,
}

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Read(path.into(), e))?;
        let mut cfg: Config = toml::from_str(&text).map_err(|e| ConfigError::Parse(path.into(), e.to_string()))?;
        if let Some(tf) = cfg.auth.token_file.clone() {
            let tf = if tf.is_relative() { path.parent().unwrap_or(std::path::Path::new(".")).join(tf) } else { tf };
            cfg.auth.token_file = Some(tf.clone());
            if !tf.exists() {
                // A missing token file is created empty on first use by the admin API.
                cfg.validate()?;
                return Ok(cfg);
            }
            let t = std::fs::read_to_string(&tf).map_err(|e| ConfigError::Read(tf.clone(), e))?;
            let f: TokenFile = toml::from_str(&t).map_err(|e| ConfigError::Parse(tf.clone(), e.to_string()))?;
            cfg.auth.tokens.extend(f.tokens);
        }
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let bad = |m: String| Err(ConfigError::Invalid(m));
        if self.server.max_concurrent_requests == 0 {
            return bad("server.max_concurrent_requests must be >= 1".into());
        }
        if let Some(r) = self.server.rate_limit
            && !(r.per_sec > 0.0 && r.burst >= 1.0 && r.per_sec.is_finite() && r.burst.is_finite())
        {
            return bad("server.rate_limit needs per_sec > 0 and burst >= 1".into());
        }
        if self.server.request_timeout_secs == 0 {
            return bad("server.request_timeout_secs must be >= 1".into());
        }
        if self.gc.grace_secs < 600 {
            return bad("gc.grace_secs must be >= 600 (it is also the put->replace lease)".into());
        }
        if !(0.0..=1.0).contains(&self.gc.min_garbage_ratio) {
            return bad("gc.min_garbage_ratio must be in [0, 1]".into());
        }
        if let BackendSection::S3(s3) = &self.backend
            && !s3.conditional_writes
            && self.repo.refs == pico2peta_repo::RefsStore::Backend
        {
            return bad("repo.refs = \"backend\" needs backend.conditional_writes = true (the ref log's commit point must be an atomic create)".into());
        }
        if let Some(o) = &self.auth.oidc {
            let local = |u: &str| u.starts_with("http://127.0.0.1") || u.starts_with("http://localhost");
            for u in std::iter::once(&o.issuer).chain(o.jwks_url.iter()) {
                if !(u.starts_with("https://") || local(u)) {
                    return bad(format!("auth.oidc: {u:?} must be https"));
                }
            }
            if o.audience.is_empty() || o.principal_claim.is_empty() {
                return bad("auth.oidc: audience and principal_claim must be set".into());
            }
        }
        let mut seen = std::collections::HashSet::new();
        for t in &self.auth.tokens {
            if t.sha256.len() != 64 || !t.sha256.bytes().all(|c| c.is_ascii_hexdigit()) {
                return bad(format!("token for {:?}: sha256 must be 64 hex chars", t.principal));
            }
            if !valid_principal(&t.principal) {
                return bad(format!("invalid principal name {:?}", t.principal));
            }
            if !seen.insert(t.sha256.to_ascii_lowercase()) {
                return bad(format!("duplicate token hash for {:?}", t.principal));
            }
        }
        for p in &self.policy {
            if !(p.principal == "*" || p.principal == crate::auth::ANONYMOUS || valid_principal(&p.principal)) {
                return bad(format!("policy: invalid principal {:?}", p.principal));
            }
            if !p.namespace.is_empty() {
                pico2peta_core::validate_path(&p.namespace)
                    .map_err(|e| ConfigError::Invalid(format!("policy namespace {:?}: {e}", p.namespace)))?;
            }
        }
        Ok(())
    }
}

/// Principal names: `[A-Za-z0-9._@:-]{1,128}`, not the reserved `anonymous`/`*`.
pub fn valid_principal(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 128
        && p != crate::auth::ANONYMOUS
        && p.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'@' | b':' | b'-'))
}

#[cfg(test)]
mod upstream_tests {
    use super::Upstream;

    #[test]
    fn upstream_fetches_stay_on_allowed_hosts() {
        let up = |url: &str, hosts: &[&str]| Upstream {
            url: url.into(),
            token: None,
            ttl_secs: 1,
            file_hosts: hosts.iter().map(|s| s.to_string()).collect(),
        };
        let pypi = up("https://pypi.org/simple", &[]);
        assert!(pypi.allows("https://files.pythonhosted.org/packages/x.whl"));
        assert!(pypi.allows("https://PyPI.org/simple/x/"));
        assert!(!pypi.allows("http://169.254.169.254/latest/meta-data/"), "IMDS");
        assert!(!pypi.allows("http://127.0.0.1:8080/admin"));
        assert!(!pypi.allows("file:///etc/passwd"));
        assert!(!pypi.allows("https://files.pythonhosted.org.evil.example/x"));
        let npm = up("https://registry.npmjs.org", &["cdn.example.com"]);
        assert!(npm.allows("https://cdn.example.com/a.tgz") && !npm.allows("https://other.example.com/a.tgz"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[server]
listen = "127.0.0.1:0"
[repo]
path = "/tmp/x"
create = true
[backend]
type = "s3"
endpoint = "http://127.0.0.1:9000"
region = "us-east-1"
bucket = "b"
[auth]
[[auth.tokens]]
principal = "ci"
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
[[policy]]
principal = "ci"
namespace = "pkgs"
access = "write"
[audit]
path = "/tmp/audit.jsonl"
"#;

    #[test]
    fn parses_sample() {
        let c: Config = toml::from_str(SAMPLE).unwrap();
        c.validate().unwrap();
        assert!(matches!(c.backend, BackendSection::S3(ref s) if s.path_style));
        assert_eq!(c.policy[0].access, Access::Write);
    }

    #[test]
    fn rejects_unknown_fields_and_bad_values() {
        assert!(toml::from_str::<Config>(&SAMPLE.replace("create = true", "creat = true")).is_err());
        let c: Config = toml::from_str(&SAMPLE.replace("namespace = \"pkgs\"", "namespace = \"../x\"")).unwrap();
        assert!(c.validate().is_err());
        let c: Config =
            toml::from_str(&SAMPLE.replace("principal = \"ci\"\nsha", "principal = \"anonymous\"\nsha")).unwrap();
        assert!(c.validate().is_err());
    }
}
