//! Default credential chain against mock IMDS and STS servers.

use o5s_backend_s3::CredentialsProvider;
use o5s_backend_s3::credentials::{RefreshingCredentials, Source, fetch};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn rfc3339(t: SystemTime) -> String {
    // Seconds since epoch -> civil (enough for tests).
    let s = t.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() as i64;
    let z = s / 86_400 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let r = s % 86_400;
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", r / 3600, r / 60 % 60, r % 60)
}

/// A tiny HTTP server answering with `route(method, path, headers, body) -> (status, body)`.
async fn mock<F>(route: F) -> String
where
    F: Fn(&str, &str, &str, &str) -> (u16, String) + Send + Sync + 'static,
{
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let route = Arc::new(route);
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            let route = route.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                loop {
                    let n = s.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..i]).to_string();
                        let cl: usize = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        while buf.len() < i + 4 + cl {
                            let n = s.read(&mut tmp).await.unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            buf.extend_from_slice(&tmp[..n]);
                        }
                        let body = String::from_utf8_lossy(&buf[i + 4..]).to_string();
                        let mut first = head.lines().next().unwrap_or("").split(' ');
                        let (m, p) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
                        let (status, out) = route(m, p, &head.to_ascii_lowercase(), &body);
                        let resp = format!(
                            "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}",
                            out.len()
                        );
                        let _ = s.write_all(resp.as_bytes()).await;
                        return;
                    }
                }
            });
        }
    });
    format!("http://{addr}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn imds_v2_and_refresh() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c2 = calls.clone();
    let ep = mock(move |m, p, h, _| match (m, p) {
        ("PUT", "/latest/api/token") if h.contains("x-aws-ec2-metadata-token-ttl-seconds") => (200, "TOKEN".into()),
        ("GET", _) if !h.contains("x-aws-ec2-metadata-token: token") => (401, "no token".into()),
        ("GET", "/latest/meta-data/iam/security-credentials/") => (200, "my-role\n".into()),
        ("GET", "/latest/meta-data/iam/security-credentials/my-role") => {
            let n = c2.fetch_add(1, Ordering::SeqCst);
            // First credentials expire almost immediately (inside the refresh window).
            let exp = SystemTime::now() + if n <= 1 { Duration::from_secs(301) } else { Duration::from_secs(3600) };
            (200, format!(r#"{{"Code":"Success","AccessKeyId":"AK{n}","SecretAccessKey":"S{n}","Token":"T{n}","Expiration":"{}"}}"#, rfc3339(exp)))
        }
        _ => (404, String::new()),
    })
    .await;
    let src = Source::Imds { endpoint: ep };
    let t = fetch(&src).await.unwrap();
    assert_eq!(t.creds.access_key_id, "AK0");
    assert_eq!(t.creds.session_token.as_deref(), Some("T0"));
    let p = RefreshingCredentials::start(src, &tokio::runtime::Handle::current()).await.unwrap();
    assert_eq!(p.credentials().unwrap().access_key_id, "AK1");
    // AK1 expires in 301 s, so the refresher fires after ~1 s.
    let mut refreshed = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if p.credentials().unwrap().access_key_id == "AK2" {
            refreshed = true;
            break;
        }
    }
    assert!(refreshed, "credentials were not refreshed before expiry");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn imds_without_role_is_a_clear_error() {
    let ep = mock(|m, p, _, _| match (m, p) {
        ("PUT", _) => (200, "TOKEN".into()),
        _ => (404, "<html>404 - Not Found</html>".into()),
    })
    .await;
    let e = fetch(&Source::Imds { endpoint: ep }).await.unwrap_err();
    assert!(e.to_string().contains("IMDS"), "{e}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_identity() {
    let d = tempfile::tempdir().unwrap();
    let tf = d.path().join("token");
    std::fs::write(&tf, "eyJ.jwt.token\n").unwrap();
    let exp = rfc3339(SystemTime::now() + Duration::from_secs(3600));
    let ep = mock(move |m, _, _, body| {
        if m == "POST" && body.contains("Action=AssumeRoleWithWebIdentity") && body.contains("WebIdentityToken=eyJ.jwt.token")
            && body.contains("RoleArn=arn%3Aaws%3Aiam%3A%3A123%3Arole%2Fr")
        {
            (200, format!("<AssumeRoleWithWebIdentityResponse><AssumeRoleWithWebIdentityResult><Credentials><AccessKeyId>ASIA1</AccessKeyId><SecretAccessKey>sec</SecretAccessKey><SessionToken>tok</SessionToken><Expiration>{exp}</Expiration></Credentials></AssumeRoleWithWebIdentityResult></AssumeRoleWithWebIdentityResponse>"))
        } else {
            (403, "<ErrorResponse><Error><Code>AccessDenied</Code></Error></ErrorResponse>".into())
        }
    })
    .await;
    let src = Source::WebIdentity {
        token_file: tf,
        role_arn: "arn:aws:iam::123:role/r".into(),
        session_name: "s".into(),
        sts_endpoint: ep,
    };
    let t = fetch(&src).await.unwrap();
    assert_eq!(t.creds.access_key_id, "ASIA1");
    assert_eq!(t.creds.session_token.as_deref(), Some("tok"));
    assert!(t.expires.is_some());
}
