//! Segment maps (external id → core hash): the core's `pico2peta_repo::idmap`, which started
//! here. The on-disk format (`P2PSEG01` segments, `P2PMAP01` roots) is unchanged. This module
//! re-exports it and adds the async wrappers that the server's handlers use.

pub use pico2peta_repo::idmap::*;

use pico2peta_core::{Error, Result};
use std::future::Future;
use std::sync::Arc;

/// Async access to a shared map: runs on the blocking pool. In-process writers are
/// serialized inside `insert_blocking`; other processes are handled by OCC.
pub trait AsyncMap {
    fn insert(&self, entries: Vec<(Key, Value)>) -> impl Future<Output = Result<usize>> + Send;
    fn get_live(&self, key: Key) -> impl Future<Output = Result<Option<Value>>> + Send;
}

fn join_err(e: tokio::task::JoinError) -> Error {
    Error::Backend { transient: false, msg: format!("segmap task: {e}") }
}

impl AsyncMap for Arc<SegMap> {
    fn insert(&self, entries: Vec<(Key, Value)>) -> impl Future<Output = Result<usize>> + Send {
        let this = self.clone();
        async move { tokio::task::spawn_blocking(move || this.insert_blocking(entries)).await.map_err(join_err)? }
    }

    fn get_live(&self, key: Key) -> impl Future<Output = Result<Option<Value>>> + Send {
        let this = self.clone();
        async move { tokio::task::spawn_blocking(move || this.get_live_blocking(&key)).await.map_err(join_err)? }
    }
}
