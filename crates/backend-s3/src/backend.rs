//! [`S3Backend`]: the synchronous `o5s_core::backend::Backend` adapter.
//!
//! The backend owns a small dedicated tokio runtime. Every trait call spawns
//! its future onto that runtime and waits on a channel, so it works from any
//! thread, including tokio `spawn_blocking` threads. It must not be called
//! directly on an async worker thread (that would block the worker); async
//! callers should use [`S3Backend::client`] or `spawn_blocking`.

use crate::client::{Body, Created, S3Client};
use crate::config::{CredentialsProvider, S3Config};
use crate::error::S3Error;
use crate::retry::backoff;
use bytes::Bytes;
use o5s_core::backend::{Backend, BlobMeta, PutOutcome, PutSource, validate_key};
use o5s_core::{Error, Result};
use std::future::Future;
use std::io::Read;
use std::ops::Range;
use std::sync::Arc;
use tracing::warn;

/// S3 / S3-compatible implementation of the core `Backend` trait.
pub struct S3Backend {
    client: S3Client,
    rt: Option<tokio::runtime::Runtime>,
}

/// Converts client errors to the core error type.
pub fn to_core(e: S3Error) -> Error {
    match e {
        S3Error::NotFound(k) => Error::KeyNotFound(k),
        S3Error::InvalidKey(k, why) => Error::Parse(format!("invalid backend key {k:?}: {why}")),
        e @ S3Error::ShortRead { .. } => Error::Corrupt(e.to_string()),
        S3Error::LocalIo(io) => Error::Io(io),
        e => Error::Backend { transient: e.is_transient(), msg: e.to_string() },
    }
}

impl S3Backend {
    /// Builds the client and its runtime (`worker_threads` I/O threads; 2 is plenty
    /// for most deployments since the work is network-bound).
    pub fn new(cfg: S3Config, creds: Arc<dyn CredentialsProvider>, worker_threads: usize) -> Result<S3Backend> {
        let client = S3Client::new(cfg, creds).map_err(to_core)?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(worker_threads.max(1))
            .thread_name("o5s-s3")
            .enable_all()
            .build()?;
        Ok(S3Backend { client, rt: Some(rt) })
    }

    /// Like [`S3Backend::new`] with the default credential chain (env, web
    /// identity, IMDSv2), with temporary credentials refreshed in the background.
    pub fn with_default_chain(cfg: S3Config, worker_threads: usize) -> Result<S3Backend> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(worker_threads.max(1))
            .thread_name("o5s-s3")
            .enable_all()
            .build()?;
        let handle = rt.handle().clone();
        let region = cfg.region.clone();
        let creds = rt.block_on(crate::credentials::default_chain(&region, &handle)).map_err(to_core)?;
        let client = S3Client::new(cfg, creds).map_err(to_core)?;
        Ok(S3Backend { client, rt: Some(rt) })
    }

    /// The async client (shares the connection pool and counters).
    pub fn client(&self) -> &S3Client {
        &self.client
    }

    /// Runs `fut` on the backend runtime and waits for it.
    fn block<T, F>(&self, fut: F) -> Result<T>
    where
        T: Send + 'static,
        F: Future<Output = std::result::Result<T, S3Error>> + Send + 'static,
    {
        let rt = self.rt.as_ref().ok_or_else(|| to_core(S3Error::Shutdown))?;
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        rt.spawn(async move {
            let _ = tx.send(fut.await);
        });
        rx.recv().map_err(|_| to_core(S3Error::Shutdown))?.map_err(to_core)
    }
}

impl Drop for S3Backend {
    fn drop(&mut self) {
        if let Some(rt) = self.rt.take() {
            // Never blocks, so dropping is safe from any context.
            rt.shutdown_background();
        }
    }
}

impl Backend for S3Backend {
    fn put_if_absent(&self, key: &str, src: PutSource<'_>) -> Result<PutOutcome> {
        validate_key(key)?;
        let body = match src {
            PutSource::Bytes(b) => Body::Bytes(Bytes::copy_from_slice(b)),
            PutSource::File(p) => Body::File(p.to_path_buf()),
        };
        let c = self.client.clone();
        let key = key.to_string();
        let r = self.block(async move { c.put_if_absent(&key, body).await })?;
        Ok(match r {
            Created::Created => PutOutcome::Created,
            Created::AlreadyExists => PutOutcome::AlreadyExists,
        })
    }

    fn get_range(&self, key: &str, range: Range<u64>) -> Result<Vec<u8>> {
        validate_key(key)?;
        if range.start > range.end {
            return Err(Error::Parse(format!("bad range {range:?}")));
        }
        let c = self.client.clone();
        let key = key.to_string();
        self.block(async move { c.get_range(&key, range).await.map(|b| b.to_vec()) })
    }

    fn open(&self, key: &str) -> Result<Box<dyn Read + Send>> {
        validate_key(key)?;
        let rt = self.rt.as_ref().ok_or_else(|| to_core(S3Error::Shutdown))?;
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let (start_tx, start_rx) = std::sync::mpsc::sync_channel(1);
        rt.spawn(stream_task(self.client.clone(), key.to_string(), tx, start_tx));
        start_rx.recv().map_err(|_| to_core(S3Error::Shutdown))?.map_err(to_core)?;
        Ok(Box::new(ObjectReader { rx, cur: Bytes::new(), done: false }))
    }

    fn head(&self, key: &str) -> Result<Option<BlobMeta>> {
        validate_key(key)?;
        let c = self.client.clone();
        let k = key.to_string();
        let m = self.block(async move { c.head(&k).await })?;
        Ok(m.map(|m| BlobMeta { key: key.to_string(), len: m.len }))
    }

    fn list(&self, prefix: &str) -> Result<Vec<BlobMeta>> {
        let c = self.client.clone();
        let p = prefix.to_string();
        let l = self.block(async move { c.list(&p).await })?;
        Ok(l.into_iter().filter(|(k, _)| validate_key(k).is_ok()).map(|(key, len)| BlobMeta { key, len }).collect())
    }

    fn delete(&self, key: &str) -> Result<()> {
        validate_key(key)?;
        let c = self.client.clone();
        let key = key.to_string();
        self.block(async move { c.delete(&key).await })
    }

    fn describe(&self) -> String {
        self.client.describe()
    }

    /// Only `If-None-Match` creates are atomic; HEAD-then-PUT is not (API v1.9).
    fn atomic_create(&self) -> bool {
        self.client.config().conditional_writes
    }
}

/// Streams `key` into `tx`, resuming with a ranged GET (pinned by `If-Match`
/// on the first response's ETag) after transient mid-body failures.
async fn stream_task(
    c: S3Client,
    key: String,
    tx: tokio::sync::mpsc::Sender<std::result::Result<Bytes, S3Error>>,
    start: std::sync::mpsc::SyncSender<std::result::Result<(), S3Error>>,
) {
    let mut start = Some(start);
    let mut pos: u64 = 0;
    let mut total: Option<u64> = None;
    let mut etag: Option<String> = None;
    let mut failures: u32 = 0;
    let rc = c.config().retry.clone();
    let fail = |start: &mut Option<std::sync::mpsc::SyncSender<_>>, e: S3Error| {
        let tx = tx.clone();
        let st = start.take();
        async move {
            match st {
                Some(st) => {
                    let _ = st.send(Err(e));
                }
                None => {
                    let _ = tx.send(Err(e)).await;
                }
            }
        }
    };
    loop {
        let err = match c.get_stream_from(&key, pos, etag.as_deref()).await {
            Err(e) => e,
            Ok((mut resp, _permit)) => {
                if start.is_some() {
                    etag = resp.headers().get("etag").and_then(|v| v.to_str().ok()).map(str::to_string);
                    total = resp.content_length();
                    if let Some(st) = start.take() {
                        let _ = st.send(Ok(()));
                    }
                }
                let mut err = None;
                loop {
                    match resp.chunk().await {
                        Ok(Some(chunk)) => {
                            pos += chunk.len() as u64;
                            c.note_received(chunk.len());
                            failures = 0;
                            if tx.send(Ok(chunk)).await.is_err() {
                                return; // reader dropped
                            }
                        }
                        Ok(None) => {
                            if total.is_some_and(|t| pos < t) {
                                err = Some(S3Error::Transport(format!("body ended at {pos} of {total:?}")));
                            }
                            break;
                        }
                        Err(e) => {
                            err = Some(S3Error::from_reqwest(e, c.config().request_timeout));
                            break;
                        }
                    }
                }
                match err {
                    None => return, // complete; dropping tx signals EOF
                    Some(e) => e,
                }
            }
        };
        if !err.is_transient() || failures + 1 >= rc.max_attempts {
            let err = if failures > 0 && err.is_transient() {
                S3Error::RetriesExhausted { attempts: failures + 1, last: Box::new(err) }
            } else {
                err
            };
            fail(&mut start, err).await;
            return;
        }
        let d = backoff(&rc, failures, &err);
        warn!(key, pos, failures, error = %err, "s3 stream: resuming");
        failures += 1;
        tokio::time::sleep(d).await;
    }
}

/// Blocking `Read` over the chunks produced by [`stream_task`].
struct ObjectReader {
    rx: tokio::sync::mpsc::Receiver<std::result::Result<Bytes, S3Error>>,
    cur: Bytes,
    done: bool,
}

impl Read for ObjectReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.cur.is_empty() {
            if self.done {
                return Ok(0);
            }
            match self.rx.blocking_recv() {
                Some(Ok(b)) => self.cur = b,
                Some(Err(e)) => {
                    self.done = true;
                    return Err(e.into());
                }
                None => {
                    self.done = true;
                    return Ok(0);
                }
            }
        }
        let n = buf.len().min(self.cur.len());
        buf[..n].copy_from_slice(&self.cur[..n]);
        let _ = self.cur.split_to(n);
        Ok(n)
    }
}
