//! The server's catalog as the launcher sees it (INS-02): pages, genres,
//! package details with this machine's release and compat layer, and install
//! plans. Release selection (09 §1) lives here, not in `compat/`: compat
//! profiles only enrich a choice this module makes, through
//! [`compat::CompatProvider`] (GAME-07).
//!
//! A small in-memory cache keeps pages and details. It has no timers: a first
//! page (`cursor: null`) is always fetched again and drops the cached pages of
//! that query, `package_details` always refreshes its package, and everything
//! is forgotten on a server switch, a sign-in or a sign-out.

pub mod compat;
pub mod covers;
pub mod release;
pub mod types;

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, RwLock};

use reqwest::Method;
use uuid::Uuid;
use vgames_core::manifest::Platform as Host;
use vgames_proto::packages::Platform as ApiPlatform;
use vgames_proto::packages::{GenreList, PackageDetail, PackagePage, ReleaseInfo};
use vgames_proto::versions::ReleaseDescriptor;

use self::compat::{CompatProvider, NoProfiles, compat_info};
use self::covers::Covers;
pub use self::types::*;
use crate::api::{ApiClient, ApiError};
use crate::db::{self, Db};
use crate::events::{AppEvent, EventBus, PackageRef};
use crate::servers::Servers;

/// Packages per catalog page.
pub const PAGE_SIZE: u32 = 48;
/// Longest title search the API accepts.
const MAX_QUERY_CHARS: usize = 100;
const MAX_GENRE_CHARS: usize = 64;
const MAX_CURSOR_BYTES: usize = 1024;
/// Cached pages and details kept at most (each map).
const MAX_CACHED: usize = 256;

/// Where API clients come from: the active server, or a given one.
pub trait Connections: Send + Sync + 'static {
    fn active(&self) -> impl Future<Output = Result<ApiClient, CatalogError>> + Send;
    fn client(
        &self,
        server_id: Uuid,
    ) -> impl Future<Output = Result<ApiClient, CatalogError>> + Send;
}

impl Connections for Servers {
    async fn active(&self) -> Result<ApiClient, CatalogError> {
        let id = self
            .active_id()
            .await
            .map_err(|e| local("read the active server", &e))?
            .ok_or(CatalogError::Unauthenticated)?;
        self.client(id).await
    }

    async fn client(&self, server_id: Uuid) -> Result<ApiClient, CatalogError> {
        self.api(server_id).await.map_err(|error| match error {
            crate::error::AppError::NotFound => CatalogError::NotFound,
            other => local("open the server", &other),
        })
    }
}

fn local(context: &str, error: &dyn std::error::Error) -> CatalogError {
    tracing::error!(error = %crate::error::DisplayChain(error), "cannot {context}");
    CatalogError::Server {
        code: "internal".into(),
        message: format!("Cannot {context}. See the log for details."),
    }
}

impl From<ApiError> for CatalogError {
    fn from(error: ApiError) -> Self {
        match error {
            ApiError::Timeout | ApiError::Network(_) | ApiError::Tls(_) => Self::Offline,
            ApiError::Unauthenticated => Self::Unauthenticated,
            ApiError::Problem { status: 404, .. } => Self::NotFound,
            ApiError::Problem { code, message, .. } => Self::Server { code, message },
            ApiError::TrustBlocked => Self::Server {
                code: "trust_blocked".into(),
                message: "The server's identity changed; it is blocked".into(),
            },
            ApiError::InvalidResponse(message) => Self::Server {
                code: "invalid_response".into(),
                message,
            },
        }
    }
}

/// What `install_start` (INS-03) needs after a successful plan.
#[derive(Debug, Clone)]
pub struct PlannedInstall {
    pub package: PackageRef,
    pub detail: Arc<PackageDetail>,
    pub release: ReleaseInfo,
    pub route: release::Route,
    pub plan: InstallPlan,
}

type PageKey = (Uuid, CatalogQuery);

#[derive(Default)]
struct Cache {
    pages: HashMap<PageKey, Arc<PackagePage>>,
    details: HashMap<(Uuid, Uuid), Arc<PackageDetail>>,
}

pub struct Catalog<C: Connections = Servers> {
    connections: Arc<C>,
    db: Db,
    covers: Arc<Covers>,
    compat: RwLock<Arc<dyn CompatProvider>>,
    host: Option<Host>,
    cache: Mutex<Cache>,
}

impl<C: Connections> Catalog<C> {
    pub fn new(connections: Arc<C>, db: Db, covers: Arc<Covers>, host: Option<Host>) -> Self {
        Self {
            connections,
            db,
            covers,
            compat: RwLock::new(Arc::new(NoProfiles)),
            host,
            cache: Mutex::new(Cache::default()),
        }
    }

    pub fn connections(&self) -> &Arc<C> {
        &self.connections
    }

    pub fn covers(&self) -> &Arc<Covers> {
        &self.covers
    }

    /// GAME-07 registers signed compat profiles here.
    pub fn set_compat_provider(&self, provider: Arc<dyn CompatProvider>) {
        if let Ok(mut slot) = self.compat.write() {
            *slot = provider;
        }
    }

    fn compat_provider(&self) -> Arc<dyn CompatProvider> {
        match self.compat.read() {
            Ok(slot) => Arc::clone(&slot),
            Err(_) => Arc::new(NoProfiles),
        }
    }

    /// Forgets every cached page, detail and image registration.
    pub fn invalidate(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            *cache = Cache::default();
        }
        self.covers.forget_all();
    }

    /// Invalidates the cache on server switches and sign-in/out, until `stop`.
    pub fn spawn_invalidation(
        self: &Arc<Self>,
        bus: &EventBus,
        stop: tokio_util::sync::CancellationToken,
    ) {
        let mut events = bus.subscribe();
        let catalog = Arc::downgrade(self);
        tauri::async_runtime::spawn(async move {
            loop {
                let event = tokio::select! {
                    () = stop.cancelled() => return,
                    event = events.recv() => event,
                };
                let relevant = match event {
                    Ok(
                        AppEvent::ServerSwitched(_)
                        | AppEvent::ServersChanged(_)
                        | AppEvent::AuthFinished(_),
                    ) => true,
                    Ok(_) => false,
                    // Missed events may have been any of the above.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => true,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                };
                match catalog.upgrade() {
                    Some(catalog) if relevant => catalog.invalidate(),
                    Some(_) => {}
                    None => return,
                }
            }
        });
    }

    fn route(&self, platforms: &[ApiPlatform]) -> Option<(ApiPlatform, release::Route)> {
        self.host
            .and_then(|host| release::route_for(platforms, host))
    }

    pub async fn list(&self, query: CatalogQuery) -> Result<CatalogPage, CatalogError> {
        let query = validate(query)?;
        let client = self.connections.active().await?;
        let server_id = client.session().server_id();
        let key = (server_id, query.clone());
        let cached = match &query.cursor {
            Some(_) => self
                .cache
                .lock()
                .ok()
                .and_then(|c| c.pages.get(&key).cloned()),
            // A first page is a refresh of that query.
            None => {
                if let Ok(mut cache) = self.cache.lock() {
                    cache.pages.retain(|(server, q), _| {
                        *server != server_id
                            || q.query != query.query
                            || q.genre != query.genre
                            || q.sort != query.sort
                    });
                }
                None
            }
        };
        let page = match cached {
            Some(page) => page,
            None => {
                let page: Arc<PackagePage> = Arc::new(
                    client
                        .authed(Method::GET, &list_path(&query), None::<&()>)
                        .await?,
                );
                if let Ok(mut cache) = self.cache.lock() {
                    if cache.pages.len() >= MAX_CACHED {
                        cache.pages.clear();
                    }
                    cache.pages.insert(key, Arc::clone(&page));
                }
                page
            }
        };
        let items = page
            .items
            .iter()
            .map(|summary| CatalogItem {
                package_id: summary.id.to_string(),
                slug: summary.slug.clone(),
                title: summary.title.clone(),
                summary: summary.summary.clone(),
                genres: summary.genres.clone(),
                cover_url: summary
                    .cover
                    .as_ref()
                    .and_then(|a| self.covers.url(server_id, a.id)),
                platforms: summary.platforms.iter().map(|p| (*p).into()).collect(),
                availability: Availability::of(self.route(&summary.platforms).map(|(_, r)| r)),
                updated_at: rfc3339(summary.updated_at),
            })
            .collect();
        Ok(CatalogPage {
            items,
            next_cursor: page.next_cursor.clone(),
        })
    }

    pub async fn genres(&self) -> Result<Vec<GenreCount>, CatalogError> {
        let client = self.connections.active().await?;
        let list: GenreList = client.authed(Method::GET, "v1/genres", None::<&()>).await?;
        Ok(list
            .items
            .into_iter()
            .map(|g| GenreCount {
                genre: g.genre,
                count: u32::try_from(g.count).unwrap_or(u32::MAX),
            })
            .collect())
    }

    async fn detail(
        &self,
        client: &ApiClient,
        package_id: Uuid,
        refresh: bool,
    ) -> Result<Arc<PackageDetail>, CatalogError> {
        let key = (client.session().server_id(), package_id);
        if !refresh
            && let Some(detail) = self
                .cache
                .lock()
                .ok()
                .and_then(|c| c.details.get(&key).cloned())
        {
            return Ok(detail);
        }
        let result = client
            .authed::<(), PackageDetail>(Method::GET, &format!("v1/packages/{package_id}"), None)
            .await;
        let mut cache = self.cache.lock().ok();
        match result {
            Ok(detail) => {
                // A server must not answer with another package.
                if detail.summary.id != package_id {
                    return Err(CatalogError::Server {
                        code: "invalid_response".into(),
                        message: "The server answered with another package".into(),
                    });
                }
                let detail = Arc::new(detail);
                if let Some(cache) = cache.as_mut() {
                    if cache.details.len() >= MAX_CACHED {
                        cache.details.clear();
                    }
                    cache.details.insert(key, Arc::clone(&detail));
                }
                Ok(detail)
            }
            Err(error) => {
                if let Some(cache) = cache.as_mut() {
                    cache.details.remove(&key);
                }
                Err(error.into())
            }
        }
    }

    async fn compat_for(&self, package: PackageRef, detail: &PackageDetail) -> CompatInfo {
        let provider = self.compat_provider();
        let selected = select(&detail.releases, self.host);
        compat_info(
            provider.as_ref(),
            package,
            selected.map(|(r, route)| (r.platform, route)),
            detail.protondb_tier.map(Into::into),
        )
        .await
    }

    pub async fn details(&self, package_id: Uuid) -> Result<PackageDetails, CatalogError> {
        let client = self.connections.active().await?;
        let server_id = client.session().server_id();
        let detail = self.detail(&client, package_id, true).await?;
        let package = PackageRef {
            server_id,
            package_id,
        };
        let compat = self.compat_for(package, &detail).await;
        let summary = &detail.summary;
        let image = |asset: &Option<vgames_proto::packages::Asset>| {
            asset
                .as_ref()
                .and_then(|a| self.covers.url(server_id, a.id))
        };
        Ok(PackageDetails {
            package_id: summary.id.to_string(),
            slug: summary.slug.clone(),
            title: summary.title.clone(),
            summary: summary.summary.clone(),
            description: detail.description.clone(),
            developer: detail.developer.clone(),
            publisher: detail.publisher.clone(),
            release_date: detail
                .release_date
                .map(|d| format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())),
            genres: summary.genres.clone(),
            platforms: summary.platforms.iter().map(|p| (*p).into()).collect(),
            cover_url: image(&summary.cover),
            hero_url: image(&detail.hero),
            logo_url: image(&detail.logo),
            screenshots: detail
                .screenshots
                .iter()
                .filter_map(|a| {
                    Some(Screenshot {
                        url: self.covers.url(server_id, a.id)?,
                        width: u32::try_from(a.width).unwrap_or(0),
                        height: u32::try_from(a.height).unwrap_or(0),
                    })
                })
                .collect(),
            release: select(&detail.releases, self.host)
                .map(|(r, route)| HostRelease::new(r, route)),
            compat,
        })
    }

    /// The active server's id.
    pub async fn active_server(&self) -> Result<Uuid, CatalogError> {
        Ok(self.connections.active().await?.session().server_id())
    }

    /// Sizes for the install dialog, and everything `install_start` needs.
    pub async fn plan(&self, package_id: Uuid) -> Result<PlannedInstall, InstallPlanError> {
        let server_id = self.active_server().await?;
        self.plan_for(PackageRef {
            server_id,
            package_id,
        })
        .await
    }

    /// Like [`Self::plan`] for a package of any registered server (invites).
    pub async fn plan_for(&self, package: PackageRef) -> Result<PlannedInstall, InstallPlanError> {
        let client = self.connections.client(package.server_id).await?;
        let package_id = package.package_id;
        let detail = self.detail(&client, package_id, false).await?;
        let (release, route) = select(&detail.releases, self.host)
            .map(|(r, route)| (r.clone(), route))
            .ok_or(InstallPlanError::NoRelease)?;
        let installed = db::installs::row(&self.db, package)
            .await
            .map_err(|e| InstallPlanError::from(local("read the installs", &e)))?;
        if installed.is_some() {
            return Err(InstallPlanError::AlreadyInstalled);
        }
        if let Some(blocker) = self.compat_for(package, &detail).await.hard_blocker() {
            return Err(InstallPlanError::Blocked {
                blocker: blocker.clone(),
            });
        }
        let host_release = HostRelease::new(&release, route);
        let plan = InstallPlan {
            package_id: package_id.to_string(),
            download_bytes: host_release.total_size,
            required_bytes: host_release
                .total_size
                .saturating_add(vgames_transfer::install::SPACE_MARGIN),
            release: host_release,
        };
        Ok(PlannedInstall {
            package,
            detail,
            release,
            route,
            plan,
        })
    }

    /// The current release descriptor of a package for `platform` (signed
    /// manifest link and envelope; INS-03, INS-04).
    pub async fn descriptor(
        &self,
        package: PackageRef,
        platform: ApiPlatform,
    ) -> Result<ReleaseDescriptor, CatalogError> {
        let client = self.connections.client(package.server_id).await?;
        let descriptor: ReleaseDescriptor = client
            .authed(
                Method::GET,
                &format!(
                    "v1/packages/{}/releases/{}",
                    package.package_id,
                    platform.as_str()
                ),
                None::<&()>,
            )
            .await?;
        if descriptor.package_id != package.package_id || descriptor.platform != platform {
            return Err(CatalogError::Server {
                code: "invalid_response".into(),
                message: "The server answered with another release".into(),
            });
        }
        Ok(descriptor)
    }
}

fn select(releases: &[ReleaseInfo], host: Option<Host>) -> Option<(&ReleaseInfo, release::Route)> {
    let selected = release::select_release(releases, host?)?;
    Some((selected.release, selected.route))
}

fn validate(mut query: CatalogQuery) -> Result<CatalogQuery, CatalogError> {
    let invalid = |message: &str| CatalogError::Server {
        code: "invalid_query".into(),
        message: message.into(),
    };
    query.query = query.query.trim().to_owned();
    if query.query.chars().count() > MAX_QUERY_CHARS {
        return Err(invalid("The search is too long"));
    }
    if query
        .genre
        .as_ref()
        .is_some_and(|g| g.is_empty() || g.chars().count() > MAX_GENRE_CHARS)
    {
        return Err(invalid("The genre is invalid"));
    }
    if query
        .cursor
        .as_ref()
        .is_some_and(|c| c.is_empty() || c.len() > MAX_CURSOR_BYTES)
    {
        return Err(invalid("The page cursor is invalid"));
    }
    Ok(query)
}

fn list_path(query: &CatalogQuery) -> String {
    let mut params = url::form_urlencoded::Serializer::new(String::new());
    params.append_pair("limit", &PAGE_SIZE.to_string());
    params.append_pair("sort", query.sort.as_str());
    if !query.query.is_empty() {
        params.append_pair("q", &query.query);
    }
    if let Some(genre) = &query.genre {
        params.append_pair("genre", genre);
    }
    if let Some(cursor) = &query.cursor {
        params.append_pair("cursor", cursor);
    }
    format!("v1/packages?{}", params.finish())
}

#[cfg(test)]
mod tests;
