//! What the queue worker needs from servers: the current release descriptor,
//! the verified trust state, and pack links and integrity reports for one
//! version. [`ServerBackend`] serves them through the launcher's `ApiClient`;
//! tests use the transfer crate's loopback rig instead.

use std::future::Future;
use std::sync::Arc;

use reqwest::Method;
use uuid::Uuid;
use vgames_core::trust::TrustState;
use vgames_proto::packages::Platform;
use vgames_proto::versions::{
    DownloadUrlsRequest, IntegrityReport, PackUrl, PackUrlList, ReleaseDescriptor,
};
use vgames_transfer::download::{PackUrlSource, RemoteError};

use crate::api::{ApiClient, ApiError};
use crate::catalog::{Catalog, CatalogError, Connections};
use crate::events::PackageRef;
use crate::servers::Servers;

pub trait Backend: Send + Sync + 'static {
    type Packs: PackUrlSource;

    /// `GET /v1/packages/{id}/releases/{platform}`.
    fn descriptor(
        &self,
        package: PackageRef,
        platform: Platform,
    ) -> impl Future<Output = Result<ReleaseDescriptor, CatalogError>> + Send;

    /// The server's verified trust state; `refresh` fetches the bundle again
    /// first (an unknown signing key). `None` until a bundle was verified.
    fn trust(
        &self,
        server_id: Uuid,
        refresh: bool,
    ) -> impl Future<Output = Result<Option<Arc<TrustState>>, CatalogError>> + Send;

    /// Pack links and integrity reports for one version.
    fn packs(
        &self,
        package: PackageRef,
        version_id: Uuid,
    ) -> impl Future<Output = Result<Self::Packs, CatalogError>> + Send;

    /// Client for signed manifest and pack URLs (no credentials).
    fn transfer_http(&self) -> &reqwest::Client;
}

pub struct ServerBackend {
    catalog: Arc<Catalog<Servers>>,
    servers: Arc<Servers>,
    http: reqwest::Client,
}

impl ServerBackend {
    pub fn new(catalog: Arc<Catalog<Servers>>, servers: Arc<Servers>) -> reqwest::Result<Self> {
        Ok(Self {
            catalog,
            servers,
            http: vgames_transfer::http::transfer_client(&Default::default())?,
        })
    }
}

impl Backend for ServerBackend {
    type Packs = ApiPacks;

    async fn descriptor(
        &self,
        package: PackageRef,
        platform: Platform,
    ) -> Result<ReleaseDescriptor, CatalogError> {
        self.catalog.descriptor(package, platform).await
    }

    async fn trust(
        &self,
        server_id: Uuid,
        refresh: bool,
    ) -> Result<Option<Arc<TrustState>>, CatalogError> {
        if refresh {
            match self.servers.refresh_trust(server_id).await {
                Ok(state) => return Ok(state),
                Err(error) => {
                    tracing::warn!(%server_id, error = %crate::error::DisplayChain(&error), "cannot refresh the trust bundle");
                }
            }
        }
        self.servers.trust_state(server_id).await.map_err(|error| {
            tracing::error!(error = %crate::error::DisplayChain(&error), "cannot read the trust state");
            CatalogError::Server {
                code: "internal".into(),
                message: "Cannot read the server's trust state".into(),
            }
        })
    }

    async fn packs(&self, package: PackageRef, version_id: Uuid) -> Result<ApiPacks, CatalogError> {
        Ok(ApiPacks {
            client: self.catalog.connections().client(package.server_id).await?,
            version_id,
        })
    }

    fn transfer_http(&self) -> &reqwest::Client {
        &self.http
    }
}

/// `POST /v1/versions/{version_id}/download-urls` and `…/integrity-reports`.
pub struct ApiPacks {
    client: ApiClient,
    version_id: Uuid,
}

impl ApiPacks {
    pub fn new(client: ApiClient, version_id: Uuid) -> Self {
        Self { client, version_id }
    }
}

fn remote(error: ApiError) -> RemoteError {
    let retryable = match &error {
        ApiError::Timeout | ApiError::Network(_) => true,
        ApiError::Problem { status, .. } => *status == 429 || *status >= 500,
        _ => false,
    };
    RemoteError {
        retryable,
        code: error.problem_code().map(str::to_owned),
        message: error.to_string(),
    }
}

impl PackUrlSource for ApiPacks {
    async fn pack_urls(&self, packs: &[u32]) -> Result<Vec<PackUrl>, RemoteError> {
        let request = DownloadUrlsRequest {
            packs: packs.iter().copied().collect(),
        };
        let list: PackUrlList = self
            .client
            .authed(
                Method::POST,
                &format!("v1/versions/{}/download-urls", self.version_id),
                Some(&request),
            )
            .await
            .map_err(remote)?;
        Ok(list.items)
    }

    async fn report_integrity(&self, report: IntegrityReport) -> Result<(), RemoteError> {
        self.client
            .authed_empty(
                Method::POST,
                &format!("v1/versions/{}/integrity-reports", self.version_id),
                Some(&report),
            )
            .await
            .map_err(remote)
    }
}

/// Remembers whether an integrity report reached the server, for
/// `damaged_file { reported }`.
pub struct Reporting<P> {
    inner: P,
    reported: std::sync::atomic::AtomicBool,
}

impl<P> Reporting<P> {
    pub fn new(inner: P) -> Self {
        Self {
            inner,
            reported: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn reported(&self) -> bool {
        self.reported.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl<P: PackUrlSource> PackUrlSource for Reporting<P> {
    async fn pack_urls(&self, packs: &[u32]) -> Result<Vec<PackUrl>, RemoteError> {
        self.inner.pack_urls(packs).await
    }

    async fn report_integrity(&self, report: IntegrityReport) -> Result<(), RemoteError> {
        self.inner.report_integrity(report).await?;
        self.reported
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}
