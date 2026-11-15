//! Native TLS and SIGHUP-style reload. Needs the `openssl` CLI for test certs.

mod common;
use common::*;
use std::process::Command;
use std::time::Duration;

/// A throwaway CA and a leaf for localhost signed by it. Returns
/// (leaf cert, leaf key); the CA cert is written to `<name>.ca.crt`.
fn make_cert(dir: &std::path::Path, name: &str) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let p = |s: &str| dir.join(format!("{name}.{s}"));
    let run = |args: &[&str]| {
        Command::new("openssl").current_dir(dir).args(args).output().ok().is_some_and(|o| o.status.success())
    };
    std::fs::write(p("ext"), "basicConstraints=CA:FALSE\nsubjectAltName=DNS:localhost,IP:127.0.0.1\n").ok()?;
    let ok = run(&[
        "req",
        "-x509",
        "-newkey",
        "ec",
        "-pkeyopt",
        "ec_paramgen_curve:prime256v1",
        "-nodes",
        "-days",
        "1",
        "-subj",
        "/CN=test-ca",
        "-keyout",
        p("ca.key").to_str()?,
        "-out",
        p("ca.crt").to_str()?,
    ]) && run(&[
        "req",
        "-newkey",
        "ec",
        "-pkeyopt",
        "ec_paramgen_curve:prime256v1",
        "-nodes",
        "-subj",
        "/CN=localhost",
        "-keyout",
        p("key").to_str()?,
        "-out",
        p("csr").to_str()?,
    ]) && run(&[
        "x509",
        "-req",
        "-in",
        p("csr").to_str()?,
        "-CA",
        p("ca.crt").to_str()?,
        "-CAkey",
        p("ca.key").to_str()?,
        "-CAcreateserial",
        "-days",
        "1",
        "-extfile",
        p("ext").to_str()?,
        "-out",
        p("crt").to_str()?,
    ]);
    ok.then(|| (p("crt"), p("key")))
}

#[test]
fn https_and_hot_reload() {
    let d = tempfile::tempdir().unwrap();
    let Some((cert, key)) = make_cert(d.path(), "a") else {
        eprintln!("skipping: openssl not available");
        return;
    };
    let repo_dir = d.path().join("repo");
    let audit = d.path().join("audit.jsonl");
    let write_cfg = |tokens: &[(&str, &str)]| {
        let mut s = format!(
            "[server]\nlisten = \"127.0.0.1:0\"\ntls = {{ cert = {:?}, key = {:?} }}\n[repo]\npath = {:?}\ncreate = true\n[audit]\npath = {:?}\n[auth]\n",
            cert, key, repo_dir, audit
        );
        for (p, t) in tokens {
            s.push_str(&format!(
                "[[auth.tokens]]\nprincipal = \"{p}\"\nsha256 = \"{}\"\n",
                hex(&pico2peta_server::auth::sha256(t.as_bytes()))
            ));
        }
        for (p, _) in tokens {
            s.push_str(&format!("[[policy]]\nprincipal = \"{p}\"\nnamespace = \"\"\naccess = \"write\"\n"));
        }
        std::fs::write(d.path().join("server.toml"), s).unwrap();
    };
    write_cfg(&[("a", "tok-a"), ("b", "tok-b")]);
    let cfg_path = d.path().join("server.toml");
    let cfg = Config::load(&cfg_path).unwrap();
    let state = pico2peta_server::build_state(&cfg).unwrap();
    let rc = pico2peta_server::tls::ReloadableCert::load(cert.clone(), key.clone()).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let addr = rt.block_on(async {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let tl =
            pico2peta_server::tls::TlsListener::new(l, pico2peta_server::tls::acceptor(rc.clone()).unwrap()).unwrap();
        let st = state.clone();
        tokio::spawn(async move {
            pico2peta_server::serve_on(tl, st, Duration::from_secs(30), Duration::from_secs(2), async {
                let _ = stop_rx.await;
            })
            .await
            .unwrap();
        });
        addr
    });
    let old_pem = std::fs::read(d.path().join("a.ca.crt")).unwrap();
    let ca = reqwest::Certificate::from_pem(&old_pem).unwrap();
    let client = reqwest::blocking::Client::builder().add_root_certificate(ca).build().unwrap();
    let url = format!("https://localhost:{}", addr.port());
    let who = |tok: &str| client.get(format!("{url}/v1/whoami")).bearer_auth(tok).send().unwrap().status().as_u16();
    assert_eq!(client.get(format!("{url}/healthz")).send().unwrap().text().unwrap(), "ok");
    assert_eq!(who("tok-a"), 200);
    assert_eq!(who("tok-b"), 200);
    // Plain HTTP to the TLS port fails.
    assert!(reqwest::blocking::get(format!("http://127.0.0.1:{}/healthz", addr.port())).is_err());

    // Reload: revoke b, rotate the certificate.
    write_cfg(&[("a", "tok-a")]);
    let (cert2, key2) = make_cert(d.path(), "b").unwrap();
    // Copy over the live paths (what an operator's cert renewal does).
    std::fs::copy(&cert2, &cert).unwrap();
    std::fs::copy(&key2, &key).unwrap();
    rt.block_on(pico2peta_server::reload(&state, &cfg_path, Some(&rc))).unwrap();
    assert_eq!(who("tok-a"), 200);
    assert_eq!(who("tok-b"), 401, "revoked token must stop working");
    // New connections get the new certificate: trusting only the new cert works,
    // trusting only the old one fails.
    let only = |pem: &[u8]| {
        reqwest::blocking::Client::builder()
            .tls_built_in_root_certs(false)
            .add_root_certificate(reqwest::Certificate::from_pem(pem).unwrap())
            .build()
            .unwrap()
    };
    assert_eq!(
        only(&std::fs::read(d.path().join("b.ca.crt")).unwrap()).get(format!("{url}/healthz")).send().unwrap().status(),
        200
    );
    assert!(only(&old_pem).get(format!("{url}/healthz")).send().is_err());
    // A broken config is rejected and nothing changes.
    std::fs::write(&cfg_path, "not = [valid").unwrap();
    assert!(rt.block_on(pico2peta_server::reload(&state, &cfg_path, Some(&rc))).is_err());
    assert_eq!(who("tok-a"), 200);
    let _ = stop_tx.send(());
}
