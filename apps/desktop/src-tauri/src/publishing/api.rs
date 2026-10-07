//! The admin API calls publishing makes, on the launcher's authenticated client (one refresh on 401,
//! tokens never leave Rust). Mutations are never retried here: `publish::run` decides.

use reqwest::Method;
use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;
use vgames_proto::packages::{AdminPackage, AdminPackageCreate, AdminPackagePage};
use vgames_proto::versions::{
    FinalizeRequest, UploadTarget, Version, VersionCreate, VersionPage, YankRequest,
};
use vgames_transfer::download::RemoteError;
use vgames_transfer::upload::publish::PublishApi;

use crate::api::{ApiClient, ApiError, json_or_problem};

/// Packages listed per page.
const PAGE: u32 = 50;

pub struct AdminApi {
    pub client: ApiClient,
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
        // Problem text only: never a URL or a token (ApiError carries neither).
        message: error.to_string(),
    }
}

impl AdminApi {
    async fn send<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        idempotency_key: Option<Uuid>,
    ) -> Result<T, ApiError> {
        let url = self.client.url(path)?;
        let response = self
            .client
            .with_auth(&method, |http, token| {
                let mut request = http.request(method.clone(), url.clone()).bearer_auth(token);
                if let Some(key) = idempotency_key {
                    request = request.header("Idempotency-Key", key.to_string());
                }
                match body {
                    Some(body) => request.json(body),
                    None => request,
                }
            })
            .await?;
        json_or_problem(response).await
    }

    pub async fn packages(
        &self,
        query: Option<&str>,
        cursor: Option<&str>,
    ) -> Result<AdminPackagePage, ApiError> {
        let mut url = self.client.url("v1/admin/packages")?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("limit", &PAGE.to_string());
            if let Some(q) = query.map(str::trim).filter(|q| !q.is_empty()) {
                pairs.append_pair("q", q);
            }
            if let Some(cursor) = cursor {
                pairs.append_pair("cursor", cursor);
            }
        }
        let response = self
            .client
            .with_auth(&Method::GET, |http, token| {
                http.get(url.clone()).bearer_auth(token)
            })
            .await?;
        json_or_problem(response).await
    }

    pub async fn package_create(
        &self,
        create: &AdminPackageCreate,
        idempotency_key: Uuid,
    ) -> Result<AdminPackage, ApiError> {
        self.send(
            Method::POST,
            "v1/admin/packages",
            Some(create),
            Some(idempotency_key),
        )
        .await
    }

    pub async fn package(&self, package_id: Uuid) -> Result<AdminPackage, ApiError> {
        self.send::<(), _>(
            Method::GET,
            &format!("v1/admin/packages/{package_id}"),
            None,
            None,
        )
        .await
    }

    pub async fn versions(&self, package_id: Uuid) -> Result<VersionPage, ApiError> {
        self.send::<(), _>(
            Method::GET,
            &format!("v1/admin/packages/{package_id}/versions?limit=100"),
            None,
            None,
        )
        .await
    }

    pub async fn version(&self, version_id: Uuid) -> Result<Version, ApiError> {
        self.send::<(), _>(
            Method::GET,
            &format!("v1/admin/versions/{version_id}"),
            None,
            None,
        )
        .await
    }

    pub async fn release(&self, version_id: Uuid) -> Result<Version, ApiError> {
        self.send::<(), _>(
            Method::POST,
            &format!("v1/admin/versions/{version_id}/publish"),
            None,
            None,
        )
        .await
    }

    pub async fn yank(&self, version_id: Uuid, reason: &str) -> Result<Version, ApiError> {
        self.send(
            Method::POST,
            &format!("v1/admin/versions/{version_id}/yank"),
            Some(&YankRequest {
                reason: reason.to_owned(),
            }),
            None,
        )
        .await
    }

    pub async fn abort(&self, version_id: Uuid) -> Result<(), ApiError> {
        self.client
            .authed_empty::<()>(
                Method::DELETE,
                &format!("v1/admin/versions/{version_id}"),
                None,
            )
            .await
    }
}

impl PublishApi for AdminApi {
    async fn create_version(
        &self,
        package_id: Uuid,
        request: VersionCreate,
        idempotency_key: Uuid,
    ) -> Result<Version, RemoteError> {
        self.send(
            Method::POST,
            &format!("v1/admin/packages/{package_id}/versions"),
            Some(&request),
            Some(idempotency_key),
        )
        .await
        .map_err(remote)
    }

    async fn get_version(&self, version_id: Uuid) -> Result<Version, RemoteError> {
        self.version(version_id).await.map_err(remote)
    }

    async fn pack_upload_target(
        &self,
        version_id: Uuid,
        pack: u32,
    ) -> Result<UploadTarget, RemoteError> {
        self.send::<(), _>(
            Method::POST,
            &format!("v1/admin/versions/{version_id}/packs/{pack}/upload-session"),
            None,
            None,
        )
        .await
        .map_err(remote)
    }

    async fn manifest_upload_target(&self, version_id: Uuid) -> Result<UploadTarget, RemoteError> {
        self.send::<(), _>(
            Method::POST,
            &format!("v1/admin/versions/{version_id}/manifest-upload"),
            None,
            None,
        )
        .await
        .map_err(remote)
    }

    async fn finalize(
        &self,
        version_id: Uuid,
        request: FinalizeRequest,
    ) -> Result<Version, RemoteError> {
        self.send(
            Method::POST,
            &format!("v1/admin/versions/{version_id}/finalize"),
            Some(&request),
            None,
        )
        .await
        .map_err(remote)
    }

    async fn publish(&self, version_id: Uuid) -> Result<Version, RemoteError> {
        self.release(version_id).await.map_err(remote)
    }
}
