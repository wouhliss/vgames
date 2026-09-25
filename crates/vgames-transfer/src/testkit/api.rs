//! A mock of the two API calls the download engine makes.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use vgames_proto::versions::{IntegrityReport, PackUrl};

use super::rig::{Links, Rig};
use crate::download::{PackUrlSource, RemoteError};

pub struct MockApi {
    links: Links,
    sizes: Mutex<Vec<u64>>,
    pub url_calls: AtomicU64,
    pub reports: Mutex<Vec<IntegrityReport>>,
    /// Errors returned by the next `pack_urls` calls.
    pub url_errors: Mutex<VecDeque<RemoteError>>,
}

impl MockApi {
    pub fn new(rig: &Rig, pack_sizes: Vec<u64>) -> Arc<Self> {
        Self::with_links(rig.links(), pack_sizes)
    }

    pub fn with_links(links: Links, pack_sizes: Vec<u64>) -> Arc<Self> {
        Arc::new(Self {
            links,
            sizes: Mutex::new(pack_sizes),
            url_calls: AtomicU64::new(0),
            reports: Mutex::new(Vec::new()),
            url_errors: Mutex::new(VecDeque::new()),
        })
    }

    pub fn set_pack_sizes(&self, sizes: Vec<u64>) {
        *self.sizes.lock().unwrap() = sizes;
    }

    pub fn reports(&self) -> Vec<IntegrityReport> {
        self.reports.lock().unwrap().clone()
    }

    pub fn url_calls(&self) -> u64 {
        self.url_calls.load(Ordering::Relaxed)
    }
}

impl PackUrlSource for MockApi {
    async fn pack_urls(&self, packs: &[u32]) -> Result<Vec<PackUrl>, RemoteError> {
        self.url_calls.fetch_add(1, Ordering::Relaxed);
        if let Some(error) = self.url_errors.lock().unwrap().pop_front() {
            return Err(error);
        }
        let sizes = self.sizes.lock().unwrap().clone();
        Ok(packs
            .iter()
            .map(|p| PackUrl {
                pack_index: *p,
                url: self.links.pack_url(*p),
                size: sizes.get(*p as usize).copied().unwrap_or(0) as i64,
                expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(6),
            })
            .collect())
    }

    async fn report_integrity(&self, report: IntegrityReport) -> Result<(), RemoteError> {
        self.reports.lock().unwrap().push(report);
        Ok(())
    }
}
