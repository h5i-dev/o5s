//! Shared plumbing for package-index fronts (PyPI, npm, Maven): streaming a body into the
//! store while hashing it, create-once names, and the upstream HTTP client.

// Handlers short-circuit with a ready `Response` as the error value.
#![allow(clippy::result_large_err)]

use crate::hf::ChanRead;
use axum::body::Bytes;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use md5::Md5;
use pico2peta_core::{Error, Hash, Name, Observed, ReplaceOutcome, Repository, Result};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

pub(crate) fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, msg.into()).into_response()
}

pub(crate) fn core_err(e: Error) -> Response {
    let a = crate::error::ApiError::from(e);
    err(a.status, a.message)
}

pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> std::result::Result<T, Response> {
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(core_err(e)),
        Err(e) => Err(err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

pub(crate) fn http_client() -> std::result::Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("pico2peta/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())
}

/// Where package content goes: the front's handle, plus the concrete repo for named puts.
#[derive(Clone)]
pub(crate) struct Store {
    pub repo: Arc<dyn Repository>,
    pub local: Option<pico2peta_repo::LocalRepo>,
}

/// Digests of a stored body.
#[derive(Clone, Debug)]
pub(crate) struct Stored {
    pub hash: Hash,
    pub size: u64,
    pub md5: [u8; 16],
    pub sha1: [u8; 20],
    pub sha256: [u8; 32],
    pub sha512: [u8; 64],
}

impl Stored {
    pub fn sha256_hex(&self) -> String {
        crate::git::hex(&self.sha256)
    }
    pub fn sha1_hex(&self) -> String {
        crate::git::hex(&self.sha1)
    }
    /// Subresource-integrity form, as npm uses.
    pub fn sri_sha512(&self) -> String {
        use base64::Engine as _;
        format!("sha512-{}", base64::engine::general_purpose::STANDARD.encode(self.sha512))
    }
}

struct Hashing<R> {
    inner: R,
    md5: Md5,
    sha1: Sha1,
    sha256: Sha256,
    sha512: Sha512,
    n: u64,
}

impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let k = self.inner.read(buf)?;
        self.md5.update(&buf[..k]);
        self.sha1.update(&buf[..k]);
        self.sha256.update(&buf[..k]);
        self.sha512.update(&buf[..k]);
        self.n += k as u64;
        Ok(k)
    }
}

impl Store {
    /// Stores `src` under namespace `ns`'s encoding profile, hashing it.
    pub fn put_reader(&self, ns: &Name, src: impl Read) -> Result<Stored> {
        let mut r = Hashing {
            inner: src,
            md5: Md5::new(),
            sha1: Sha1::new(),
            sha256: Sha256::new(),
            sha512: Sha512::new(),
            n: 0,
        };
        let hash = match &self.local {
            Some(l) => l.put_stream_for(ns, &mut r)?,
            None => self.repo.put_stream(&mut r)?,
        };
        Ok(Stored {
            hash,
            size: r.n,
            md5: r.md5.finalize().into(),
            sha1: r.sha1.finalize().into(),
            sha256: r.sha256.finalize().into(),
            sha512: r.sha512.finalize().into(),
        })
    }

    /// Streams a body into the store (bounded by `limit` bytes and the idle timeout).
    pub async fn put_stream<S, E>(&self, ns: Name, mut s: S, limit: u64) -> std::result::Result<Stored, Response>
    where
        S: futures_util::Stream<Item = std::result::Result<Bytes, E>> + Unpin,
        E: std::fmt::Display,
    {
        let (tx, rx) = mpsc::channel::<std::io::Result<Bytes>>(8);
        let me = self.clone();
        let job = tokio::task::spawn_blocking(move || me.put_reader(&ns, ChanRead { rx, cur: Bytes::new() }));
        let mut n = 0u64;
        let mut failed = None;
        loop {
            let item = match tokio::time::timeout(crate::api::IDLE_TIMEOUT, s.next()).await {
                Err(_) => {
                    failed = Some("transfer idle".to_string());
                    break;
                }
                Ok(None) => break,
                Ok(Some(i)) => i,
            };
            match item {
                Ok(b) => {
                    n += b.len() as u64;
                    if n > limit {
                        failed = Some("file too large".into());
                        break;
                    }
                    if tx.send(Ok(b)).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    failed = Some(e.to_string());
                    break;
                }
            }
        }
        if let Some(m) = &failed {
            let _ = tx.send(Err(std::io::Error::other(m.clone()))).await;
        }
        drop(tx);
        let res = job.await;
        if let Some(m) = failed {
            return Err(err(StatusCode::BAD_REQUEST, m));
        }
        match res {
            Ok(Ok(x)) => Ok(x),
            Ok(Err(e)) => Err(core_err(e)),
            Err(e) => Err(err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
        }
    }
}

/// Points `name` at `record`, create-once. `Ok(Ok(true))` if created, `Ok(Ok(false))` if an
/// equivalent record (per `same`) was already there; another record is `Ok(Err(..))`.
pub(crate) fn create_once(
    repo: &dyn Repository,
    name: &Name,
    record: &[u8],
    same: &dyn Fn(&[u8]) -> bool,
) -> Result<std::result::Result<bool, String>> {
    for _ in 0..4 {
        if let Some(v) = repo.resolve(name)? {
            return Ok(if same(&repo.get(&v.hash, None)?) { Ok(false) } else { Err(format!("{name} already exists")) });
        }
        let h = repo.put(record)?;
        if let ReplaceOutcome::Ok { .. } = repo.replace(name, &[(name.clone(), Observed::Absent)], h)? {
            return Ok(Ok(true));
        }
        // Lost a race: compare with the winner.
    }
    Err(Error::Corrupt(format!("{name}: create-once did not settle")))
}

/// `<dist>.dist-info/METADATA` of a stored wheel, read with range reads (end-of-central-directory,
/// central directory, one entry), so a multi-gigabyte wheel is never loaded. `None` when the
/// wheel has no such file, or uses zip64 / an unsupported method, or it is over 16 MiB.
pub(crate) fn wheel_metadata(store: &dyn Repository, h: &Hash, size: u64) -> Result<Option<Vec<u8>>> {
    wheel_metadata_with(size, &|r| store.get(h, Some(r)))
}

/// [`wheel_metadata`] over any ranged reader (`read(range)` returns exactly those bytes).
pub(crate) fn wheel_metadata_with(
    size: u64,
    read: &dyn Fn(std::ops::Range<u64>) -> Result<Vec<u8>>,
) -> Result<Option<Vec<u8>>> {
    use std::io::Read as _;
    let u16_at = |b: &[u8], o: usize| u16::from_le_bytes([b[o], b[o + 1]]) as usize;
    let u32_at = |b: &[u8], o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as u64;
    let tail_len = size.min(22 + 65_535);
    if tail_len < 22 {
        return Ok(None);
    }
    let tail = read(size - tail_len..size)?;
    let Some(e) = (0..=tail.len() - 22).rev().find(|&i| tail[i..i + 4] == [0x50, 0x4b, 0x05, 0x06]) else {
        return Ok(None);
    };
    let (cd_size, cd_off) = (u32_at(&tail, e + 12), u32_at(&tail, e + 16));
    if cd_size == 0xffff_ffff || cd_off == 0xffff_ffff || cd_off + cd_size > size || cd_size > 64 << 20 {
        return Ok(None);
    }
    let cd = read(cd_off..cd_off + cd_size)?;
    let mut i = 0;
    while i + 46 <= cd.len() && cd[i..i + 4] == [0x50, 0x4b, 0x01, 0x02] {
        let (method, csize, usize_) = (u16_at(&cd, i + 10), u32_at(&cd, i + 20), u32_at(&cd, i + 24));
        let (nlen, xlen, clen) = (u16_at(&cd, i + 28), u16_at(&cd, i + 30), u16_at(&cd, i + 32));
        let local = u32_at(&cd, i + 42);
        let name = cd.get(i + 46..i + 46 + nlen).unwrap_or_default();
        i += 46 + nlen + xlen + clen;
        let is_meta = name.ends_with(b".dist-info/METADATA") && name.iter().filter(|&&c| c == b'/').count() == 1;
        if !is_meta {
            continue;
        }
        if usize_ > 16 << 20 || csize > 16 << 20 || local + 30 > size {
            return Ok(None);
        }
        let lh = read(local..local + 30)?;
        if lh[..4] != [0x50, 0x4b, 0x03, 0x04] {
            return Ok(None);
        }
        let start = local + 30 + u16_at(&lh, 26) as u64 + u16_at(&lh, 28) as u64;
        if start + csize > size {
            return Ok(None);
        }
        let data = read(start..start + csize)?;
        return Ok(match method {
            0 => Some(data),
            8 => {
                let mut out = Vec::with_capacity(usize_ as usize);
                flate2::read::DeflateDecoder::new(&data[..]).take(16 << 20).read_to_end(&mut out)?;
                (out.len() as u64 == usize_).then_some(out)
            }
            _ => None,
        });
    }
    Ok(None)
}
