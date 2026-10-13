//! Bounded, cancellable save requests through the existing per-server auth
//! client. POST retries retain their operation UUID; signed blob URLs stay in Rust.
use std::future::Future;
use std::time::Duration;

use crate::api::{ApiClient, ApiError, problem_from, read_capped};
use reqwest::Method;
use serde::{Serialize, de::DeserializeOwned};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_proto::saves::{
    BlobDownloadUrlList, BlobDownloadUrlsRequest, CommitSnapshotRequest, PrepareBlobsRequest,
    PrepareBlobsResponse, SaveSnapshot, SaveSnapshotPage,
};

const SAVE_BODY_LIMIT: usize = 16 * 1024 * 1024;
const REQUEST_BUDGET: Duration = Duration::from_secs(5);
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error("Cloud-save request cancelled.")]
    Cancelled,
}
#[derive(Clone)]
pub struct Client {
    api: ApiClient,
    cancel: CancellationToken,
}
impl Client {
    pub fn new(api: ApiClient, cancel: CancellationToken) -> Self {
        Self { api, cancel }
    }
    async fn bounded<T>(
        &self,
        work: impl Future<Output = Result<T, ApiError>>,
    ) -> Result<T, Error> {
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(Error::Cancelled),
            result = tokio::time::timeout(REQUEST_BUDGET, work) => result.map_err(|_| Error::Api(ApiError::Timeout))?.map_err(Error::Api),
        }
    }
    async fn request<B: Serialize + Sync, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        operation: Option<Uuid>,
    ) -> Result<T, Error> {
        let url = self.api.url(path)?;
        self.bounded(async {
            let response = self
                .api
                .with_auth(&method, |http, token| {
                    let request = http.request(method.clone(), url.clone()).bearer_auth(token);
                    let request = match operation {
                        Some(id) => request.header("Idempotency-Key", id.to_string()),
                        None => request,
                    };
                    match body {
                        Some(body) => request.json(body),
                        None => request,
                    }
                })
                .await?;
            if !response.status().is_success() {
                return Err(problem_from(response).await);
            }
            let bytes = read_capped(response, SAVE_BODY_LIMIT).await?;
            serde_json::from_slice(&bytes)
                .map_err(|_| ApiError::InvalidResponse("Invalid cloud-save response.".into()))
        })
        .await
    }
    pub async fn head(&self, package: Uuid) -> Result<Option<SaveSnapshot>, Error> {
        match self
            .request::<(), SaveSnapshot>(
                Method::GET,
                &format!("v1/saves/{package}/head"),
                None,
                None,
            )
            .await
        {
            Ok(snapshot) => {
                check_snapshot(&snapshot, package)?;
                Ok(Some(snapshot))
            }
            Err(Error::Api(error))
                if error.status() == Some(404) && error.problem_code() == Some("no_saves") =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
    pub async fn snapshot(&self, package: Uuid, snapshot: Uuid) -> Result<SaveSnapshot, Error> {
        let result = self
            .request::<(), SaveSnapshot>(
                Method::GET,
                &format!("v1/saves/{package}/snapshots/{snapshot}"),
                None,
                None,
            )
            .await?;
        check_snapshot(&result, package)?;
        if result.summary.id != snapshot {
            return Err(invalid());
        }
        Ok(result)
    }
    pub async fn history_page(
        &self,
        package: Uuid,
        cursor: Option<&str>,
    ) -> Result<SaveSnapshotPage, Error> {
        let path = format!("v1/saves/{package}/snapshots");
        let path = if let Some(cursor) = cursor {
            if cursor.len() > 8192 {
                return Err(invalid());
            }
            let query = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("cursor", cursor)
                .finish();
            format!("{path}?{query}")
        } else {
            path
        };
        let page = self
            .request::<(), SaveSnapshotPage>(Method::GET, &path, None, None)
            .await?;
        if page.items.len() > 100
            || page.items.iter().any(|item| {
                item.package_id != package || item.file_count < 0 || item.total_size < 0
            })
        {
            return Err(invalid());
        }
        Ok(page)
    }
    pub async fn prepare(
        &self,
        package: Uuid,
        blobs: &PrepareBlobsRequest,
    ) -> Result<PrepareBlobsResponse, Error> {
        self.request(
            Method::POST,
            &format!("v1/saves/{package}/blobs/prepare"),
            Some(blobs),
            None,
        )
        .await
    }
    pub async fn download_urls(
        &self,
        package: Uuid,
        blobs: &BlobDownloadUrlsRequest,
    ) -> Result<BlobDownloadUrlList, Error> {
        self.request(
            Method::POST,
            &format!("v1/saves/{package}/blobs/download-urls"),
            Some(blobs),
            None,
        )
        .await
    }
    pub async fn commit(
        &self,
        package: Uuid,
        request: &CommitSnapshotRequest,
        operation: Uuid,
    ) -> Result<SaveSnapshot, Error> {
        let snapshot = self
            .request(
                Method::POST,
                &format!("v1/saves/{package}/snapshots"),
                Some(request),
                Some(operation),
            )
            .await?;
        check_snapshot(&snapshot, package)?;
        Ok(snapshot)
    }
}
fn invalid() -> Error {
    ApiError::InvalidResponse("Invalid cloud-save response.".into()).into()
}
fn check_snapshot(snapshot: &SaveSnapshot, package: Uuid) -> Result<(), Error> {
    if snapshot.summary.package_id != package
        || snapshot.files.len() > 10_000
        || usize::try_from(snapshot.summary.file_count).ok() != Some(snapshot.files.len())
        || snapshot.summary.total_size < 0
        || snapshot.files.iter().any(|file| file.size < 0)
    {
        return Err(invalid());
    }
    let total = snapshot
        .files
        .iter()
        .try_fold(0i64, |total, file| total.checked_add(file.size))
        .ok_or_else(invalid)?;
    if total != snapshot.summary.total_size {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
