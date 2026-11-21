//! Windows stand-in for `packages`: no rule there reads package facts (`kith_kernel::secrets` is unix only), so a call
//! runs its pass once, as before the cache.
// Nothing on Windows calls the pass wrapper; it stays so the daemon's code is the same.
#![allow(dead_code)]

use std::sync::Arc;

use kith_kernel::paths::Paths;
use serde_json::Value;

use crate::error::Result;

pub(crate) type Probe = Arc<dyn Fn() + Send + Sync>;

#[derive(Debug)]
pub(crate) struct PackageCache;

impl PackageCache {
    pub(crate) fn start(_probe: Option<Probe>) -> Arc<Self> {
        Arc::new(Self)
    }
}

pub(crate) fn run_call<R>(
    _cache: &PackageCache,
    _own: &Paths,
    _params: &Value,
    mut pass: impl FnMut() -> Result<R>,
) -> Result<R> {
    pass()
}
