//! Signatures as OCI artifacts, with the real cosign and notation CLIs against the OCI front:
//! cosign (tag scheme and OCI 1.1 referrers) and notation (referrers). Skipped when the tools
//! are absent (`PICO2PETA_TOOLS`, default the shared tools dir).

mod common;

use common::*;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn tool(name: &str) -> Option<PathBuf> {
    let dir = std::env::var("PICO2PETA_TOOLS").unwrap_or_else(|_| "/mnt/scratch/pico2peta/tools/bin".into());
    let p = Path::new(&dir).join(name);
    p.exists().then_some(p)
}

struct Env {
    home: PathBuf,
}

impl Env {
    fn run(&self, bin: &Path, args: &[&str], extra: &[(&str, &str)]) -> Output {
        let mut c = Command::new(bin);
        c.current_dir(&self.home)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("config"))
            .env("XDG_CACHE_HOME", self.home.join("cache"))
            .env("DOCKER_CONFIG", self.home.join("docker"))
            .env("TUF_ROOT", self.home.join("tuf"))
            .env("TMPDIR", self.home.join("tmp"))
            .args(args);
        for (k, v) in extra {
            c.env(k, v);
        }
        let o = c.output().unwrap();
        if !o.status.success() {
            eprintln!(
                "{} {args:?} failed:\n{}\n{}",
                bin.display(),
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
        }
        o
    }
}

#[test]
fn cosign_and_notation_signatures() {
    let (Some(crane), Some(cosign), Some(notation)) = (tool("crane"), tool("cosign"), tool("notation")) else {
        eprintln!("skipping: crane/cosign/notation not installed");
        return;
    };
    let srv =
        TestServer::start(|cfg| cfg.policy.push(grant("alice", "oci/team", pico2peta_server::config::Access::Write)));
    let w = tempfile::tempdir().unwrap();
    let env = Env { home: w.path().to_path_buf() };
    for d in ["config", "cache", "docker", "tuf", "tmp"] {
        std::fs::create_dir_all(w.path().join(d)).unwrap();
    }
    let host = srv.url().trim_start_matches("http://").to_string();
    let auth = base64_basic(&format!("u:{ALICE}"));
    std::fs::write(w.path().join("docker/config.json"), format!(r#"{{"auths":{{"{host}":{{"auth":"{auth}"}}}}}}"#))
        .unwrap();

    // An image: one random layer on an empty base.
    let layer = w.path().join("layer.tar");
    make_tar(&layer, "hello.txt", b"signed content\n");
    let img = format!("{host}/team/app:v1");
    assert!(
        env.run(&crane, &["append", "--insecure", "-f", layer.to_str().unwrap(), "-t", &img], &[]).status.success()
    );
    let digest =
        String::from_utf8(env.run(&crane, &["digest", "--insecure", &img], &[]).stdout).unwrap().trim().to_string();
    let by_digest = format!("{host}/team/app@{digest}");

    // cosign: key pair, sign (no transparency log), verify. Tag scheme first.
    let pw = [("COSIGN_PASSWORD", "")];
    assert!(env.run(&cosign, &["generate-key-pair"], &pw).status.success());
    let sign = |extra: &[&str], envs: &[(&str, &str)]| {
        let mut a = vec!["sign", "--key", "cosign.key", "--tlog-upload=false", "--allow-insecure-registry", "-y"];
        a.extend_from_slice(extra);
        a.push(&by_digest);
        env.run(&cosign, &a, envs).status.success()
    };
    let verify = |extra: &[&str], envs: &[(&str, &str)]| {
        let mut a = vec!["verify", "--key", "cosign.pub", "--insecure-ignore-tlog", "--allow-insecure-registry"];
        a.extend_from_slice(extra);
        a.push(&by_digest);
        env.run(&cosign, &a, envs).status.success()
    };
    assert!(sign(&[], &pw), "cosign sign (tag scheme)");
    assert!(verify(&[], &pw), "cosign verify (tag scheme)");
    // The signature is an ordinary tag in the repo: sha256-<digest>.sig.
    let tags =
        String::from_utf8(env.run(&crane, &["ls", "--insecure", &format!("{host}/team/app")], &[]).stdout).unwrap();
    assert!(tags.contains(&format!("{}.sig", digest.replace(':', "-"))), "{tags}");
    // OCI 1.1 referrers mode.
    let exp = [("COSIGN_PASSWORD", ""), ("COSIGN_EXPERIMENTAL", "1")];
    assert!(sign(&["--registry-referrers-mode=oci-1-1"], &exp), "cosign sign (referrers)");
    assert!(verify(&["--experimental-oci11"], &exp), "cosign verify (referrers)");

    // notation: test certificate, trust policy, sign, list, verify (referrers API).
    assert!(env.run(&notation, &["cert", "generate-test", "--default", "p2p-test"], &[]).status.success());
    let policy = r#"{"version":"1.0","trustPolicies":[{"name":"p2p","registryScopes":["*"],"signatureVerification":{"level":"strict"},"trustStores":["ca:p2p-test"],"trustedIdentities":["*"]}]}"#;
    std::fs::write(w.path().join("config/notation/trustpolicy.json"), policy).unwrap();
    let login = ["login", "--insecure-registry", "-u", "u", "-p", ALICE, &host];
    assert!(env.run(&notation, &login, &[]).status.success());
    assert!(env.run(&notation, &["sign", "--insecure-registry", &by_digest], &[]).status.success(), "notation sign");
    let ls = String::from_utf8(env.run(&notation, &["ls", "--insecure-registry", &by_digest], &[]).stdout).unwrap();
    assert!(ls.contains("application/vnd.cncf.notary.signature"), "{ls}");
    assert!(
        env.run(&notation, &["verify", "--insecure-registry", &by_digest], &[]).status.success(),
        "notation verify"
    );
    // A tampered target fails verification: a different image is not covered by the signature.
    make_tar(&layer, "hello.txt", b"other content\n");
    let img2 = format!("{host}/team/app:v2");
    assert!(
        env.run(&crane, &["append", "--insecure", "-f", layer.to_str().unwrap(), "-t", &img2], &[]).status.success()
    );
    let d2 =
        String::from_utf8(env.run(&crane, &["digest", "--insecure", &img2], &[]).stdout).unwrap().trim().to_string();
    let other = format!("{host}/team/app@{d2}");
    assert!(!env.run(&notation, &["verify", "--insecure-registry", &other], &[]).status.success(), "unsigned image");
}

fn base64_basic(s: &str) -> String {
    use std::fmt::Write;
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let b = s.as_bytes();
    let mut out = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                let _ = write!(out, "=");
            }
        }
    }
    out
}

/// A one-file tar archive.
fn make_tar(path: &Path, name: &str, data: &[u8]) {
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    h[100..107].copy_from_slice(b"0000644");
    h[108..115].copy_from_slice(b"0000000");
    h[116..123].copy_from_slice(b"0000000");
    h[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
    h[136..147].copy_from_slice(b"00000000000");
    h[156] = b'0';
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|&b| b as u32).sum();
    h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    let mut out = h.to_vec();
    out.extend_from_slice(data);
    out.resize(out.len().div_ceil(512) * 512, 0);
    out.resize(out.len() + 1024, 0);
    std::fs::write(path, out).unwrap();
}
