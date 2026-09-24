//! IGDB, Steam Store, ProtonDB and umu-database clients. Every response is size-capped
//! and normalized into [`CandidateData`] before it goes anywhere near the database.

use std::{num::NonZeroU32, sync::Arc, time::Duration};

use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use reqwest::StatusCode;
use serde::{Deserialize, de::DeserializeOwned};
use time::{Date, OffsetDateTime};
use tokio::{sync::Mutex, time::Instant};
use vgames_proto::packages::{
    CandidateData, CandidateExternal, CandidateImages, MetadataSource, ProtonDbTier,
};

use super::{
    normalize::{clamp, genres, html_to_text, parse_steam_date, summarize, title_score},
    safe_fetch::{FetchPolicy, SafeFetcher},
};
use crate::config::{Config, IgdbConfig};

const MAX_JSON: usize = 4 * 1024 * 1024;
const MAX_SCREENSHOTS: usize = 8;
/// Title searches keep at most this many results per provider.
const SEARCH_RESULTS: usize = 10;
/// Steam title searches fetch details for at most this many plausible results.
const STEAM_DETAILS: usize = 3;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Provider base URLs (overridable for tests).
#[derive(Clone, Debug)]
pub struct Endpoints {
    pub twitch_token: String,
    pub igdb: String,
    pub steam_store: String,
    pub protondb: String,
    pub umu: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            twitch_token: "https://id.twitch.tv/oauth2/token".into(),
            igdb: "https://api.igdb.com/v4".into(),
            steam_store: "https://store.steampowered.com".into(),
            protondb: "https://www.protondb.com".into(),
            umu: "https://umu.openwinecomponents.org".into(),
        }
    }
}

impl Endpoints {
    /// Every endpoint on one base URL (a mock server).
    pub fn all_at(base: &str) -> Self {
        let b = base.trim_end_matches('/');
        Self {
            twitch_token: format!("{b}/oauth2/token"),
            igdb: format!("{b}/v4"),
            steam_store: b.to_string(),
            protondb: b.to_string(),
            umu: b.to_string(),
        }
    }
}

/// Requests per second allowed to each provider (per API instance).
#[derive(Clone, Copy, Debug)]
pub struct ProviderRates {
    pub igdb: NonZeroU32,
    pub steam: NonZeroU32,
}

impl Default for ProviderRates {
    fn default() -> Self {
        Self {
            igdb: NonZeroU32::MIN.saturating_add(3),
            steam: NonZeroU32::MIN,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{provider}: {detail}")]
pub struct ProviderError {
    pub provider: &'static str,
    pub detail: String,
}

fn perr(provider: &'static str, detail: impl std::fmt::Display) -> ProviderError {
    ProviderError {
        provider,
        detail: detail.to_string(),
    }
}

/// A normalized search result.
#[derive(Clone, Debug)]
pub struct Found {
    pub source: MetadataSource,
    pub external_id: i64,
    pub title: String,
    pub release_year: Option<i32>,
    pub data: CandidateData,
}

impl Found {
    pub fn score(&self, package_title: &str) -> f32 {
        title_score(&self.title, package_title)
    }
}

pub struct Providers {
    http: reqwest::Client,
    endpoints: Endpoints,
    igdb: Option<IgdbConfig>,
    steam_enabled: bool,
    token: Mutex<Option<(String, Instant)>>,
    igdb_rate: DefaultDirectRateLimiter,
    steam_rate: DefaultDirectRateLimiter,
    pub images: SafeFetcher,
}

impl Providers {
    pub fn from_config(config: &Config) -> Result<Self, reqwest::Error> {
        Self::new(
            config.igdb.clone(),
            config.steam_metadata_enabled,
            Endpoints::default(),
            ProviderRates::default(),
            FetchPolicy::production(),
        )
    }

    pub fn new(
        igdb: Option<IgdbConfig>,
        steam_enabled: bool,
        endpoints: Endpoints,
        rates: ProviderRates,
        image_policy: FetchPolicy,
    ) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("vgames-api/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            endpoints,
            igdb,
            steam_enabled,
            token: Mutex::new(None),
            igdb_rate: RateLimiter::direct(Quota::per_second(rates.igdb)),
            steam_rate: RateLimiter::direct(Quota::per_second(rates.steam)),
            images: SafeFetcher::new(image_policy)?,
        })
    }

    pub fn igdb_enabled(&self) -> bool {
        self.igdb.is_some()
    }

    pub fn steam_enabled(&self) -> bool {
        self.steam_enabled
    }

    // -- IGDB ---------------------------------------------------------------------------

    /// A Twitch app token, cached until 60 s before it expires.
    async fn igdb_token(&self, cfg: &IgdbConfig) -> Result<String, ProviderError> {
        let mut cached = self.token.lock().await;
        if let Some((tok, until)) = cached.as_ref()
            && Instant::now() < *until
        {
            return Ok(tok.clone());
        }
        #[derive(Deserialize)]
        struct Token {
            access_token: String,
            expires_in: u64,
        }
        let resp = self
            .http
            .post(&self.endpoints.twitch_token)
            .form(&[
                ("client_id", cfg.client_id.as_str()),
                ("client_secret", cfg.client_secret.expose().as_str()),
                ("grant_type", "client_credentials"),
            ])
            .send()
            .await
            .map_err(|e| perr("twitch", e.without_url()))?;
        let t: Token = read_json("twitch", resp).await?;
        let ttl = Duration::from_secs(t.expires_in.saturating_sub(60));
        *cached = Some((t.access_token.clone(), Instant::now() + ttl));
        Ok(t.access_token)
    }

    async fn igdb_query(&self, body: String) -> Result<Vec<IgdbGame>, ProviderError> {
        let Some(cfg) = &self.igdb else {
            return Ok(Vec::new());
        };
        let token = self.igdb_token(cfg).await?;
        self.igdb_rate.until_ready().await;
        let resp = self
            .http
            .post(format!("{}/games", self.endpoints.igdb))
            .header("Client-ID", &cfg.client_id)
            .bearer_auth(&token)
            .header(reqwest::header::CONTENT_TYPE, "text/plain")
            .body(body)
            .send()
            .await
            .map_err(|e| perr("igdb", e.without_url()))?;
        if resp.status() == StatusCode::UNAUTHORIZED {
            // Token revoked early: forget it so the retry fetches a new one.
            *self.token.lock().await = None;
        }
        read_json("igdb", resp).await
    }

    pub async fn igdb_search(&self, title: &str) -> Result<Vec<Found>, ProviderError> {
        let q = format!(
            "{IGDB_FIELDS} search \"{}\"; limit {SEARCH_RESULTS};",
            apicalypse_string(title)
        );
        Ok(self
            .igdb_query(q)
            .await?
            .into_iter()
            .filter_map(IgdbGame::normalize)
            .collect())
    }

    /// Games IGDB links to a Steam app.
    pub async fn igdb_by_steam_id(&self, app_id: i64) -> Result<Vec<Found>, ProviderError> {
        let q = format!(
            "{IGDB_FIELDS} where external_games.external_game_source = {IGDB_STEAM} & external_games.uid = \"{app_id}\"; limit 5;"
        );
        Ok(self
            .igdb_query(q)
            .await?
            .into_iter()
            .filter_map(IgdbGame::normalize)
            .collect())
    }

    pub async fn igdb_by_id(&self, id: i64) -> Result<Vec<Found>, ProviderError> {
        let q = format!("{IGDB_FIELDS} where id = {id}; limit 1;");
        Ok(self
            .igdb_query(q)
            .await?
            .into_iter()
            .filter_map(IgdbGame::normalize)
            .collect())
    }

    // -- Steam --------------------------------------------------------------------------

    async fn steam_get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, ProviderError> {
        self.steam_rate.until_ready().await;
        let resp = self
            .http
            .get(format!("{}{path}", self.endpoints.steam_store))
            .query(query)
            .send()
            .await
            .map_err(|e| perr("steam", e.without_url()))?;
        read_json("steam", resp).await
    }

    pub async fn steam_by_id(&self, app_id: i64) -> Result<Option<Found>, ProviderError> {
        let id = app_id.to_string();
        let mut map: std::collections::HashMap<String, SteamDetailsEnvelope> = self
            .steam_get(
                "/api/appdetails",
                &[("appids", id.as_str()), ("l", "english"), ("cc", "US")],
            )
            .await?;
        Ok(map
            .remove(&id)
            .filter(|e| e.success)
            .and_then(|e| e.data)
            .and_then(|d| d.normalize(app_id)))
    }

    /// Searches by title and fetches details for the best few results.
    pub async fn steam_search(&self, title: &str) -> Result<Vec<Found>, ProviderError> {
        #[derive(Deserialize)]
        struct Search {
            #[serde(default)]
            items: Vec<SearchItem>,
        }
        #[derive(Deserialize)]
        struct SearchItem {
            id: i64,
            name: String,
            #[serde(default, rename = "type")]
            kind: String,
        }
        let search: Search = self
            .steam_get(
                "/api/storesearch/",
                &[("term", title), ("l", "english"), ("cc", "US")],
            )
            .await?;
        let mut items: Vec<(f32, SearchItem)> = search
            .items
            .into_iter()
            .filter(|i| i.id > 0 && (i.kind.is_empty() || i.kind == "app"))
            .take(SEARCH_RESULTS)
            .map(|i| (title_score(&i.name, title), i))
            .filter(|(s, _)| *s >= 0.5)
            .collect();
        items.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut out = Vec::new();
        for (_, item) in items.into_iter().take(STEAM_DETAILS) {
            if let Some(found) = self.steam_by_id(item.id).await? {
                out.push(found);
            }
        }
        Ok(out)
    }

    // -- Hints (best effort) ------------------------------------------------------------

    pub async fn protondb_tier(&self, app_id: i64) -> Result<Option<ProtonDbTier>, ProviderError> {
        #[derive(Deserialize)]
        struct Summary {
            tier: Option<String>,
        }
        let resp = self
            .http
            .get(format!(
                "{}/api/v1/reports/summaries/{app_id}.json",
                self.endpoints.protondb
            ))
            .send()
            .await
            .map_err(|e| perr("protondb", e.without_url()))?;
        if resp.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let s: Summary = read_json("protondb", resp).await?;
        Ok(s.tier
            .as_deref()
            .and_then(|t| serde_json::from_value(serde_json::json!(t)).ok()))
    }

    pub async fn umu_id(&self, app_id: i64) -> Result<Option<String>, ProviderError> {
        #[derive(Deserialize)]
        struct Entry {
            umu_id: Option<String>,
        }
        let id = app_id.to_string();
        let resp = self
            .http
            .get(format!("{}/umu_api.php", self.endpoints.umu))
            .query(&[("store", "steam"), ("codename", id.as_str())])
            .send()
            .await
            .map_err(|e| perr("umu", e.without_url()))?;
        let entries: Vec<Entry> = read_json("umu", resp).await?;
        Ok(entries
            .into_iter()
            .filter_map(|e| e.umu_id)
            .find(|u| valid_umu_id(u)))
    }
}

fn valid_umu_id(s: &str) -> bool {
    s.len() <= 64
        && s.starts_with("umu-")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Reads a JSON body (size-capped); 5xx, 429 and 401 are errors worth retrying.
async fn read_json<T: DeserializeOwned>(
    provider: &'static str,
    resp: reqwest::Response,
) -> Result<T, ProviderError> {
    let status = resp.status();
    if !status.is_success() {
        return Err(perr(provider, format!("http status {}", status.as_u16())));
    }
    let body = super::safe_fetch::read_capped(resp, MAX_JSON)
        .await
        .map_err(|e| perr(provider, e))?;
    serde_json::from_slice(&body).map_err(|e| perr(provider, format!("unexpected response: {e}")))
}

/// Escapes a string for an Apicalypse `search "…"` clause.
fn apicalypse_string(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '"' | '\\' | ';') && !c.is_control())
        .take(100)
        .collect()
}

// -- IGDB wire format ---------------------------------------------------------------------

const IGDB_FIELDS: &str = "fields name,summary,storyline,first_release_date,genres.name,\
involved_companies.developer,involved_companies.publisher,involved_companies.company.name,\
cover.image_id,artworks.image_id,screenshots.image_id,\
external_games.category,external_games.external_game_source,external_games.uid;";

/// IGDB's external game source for Steam.
const IGDB_STEAM: i64 = 1;

#[derive(Debug, Deserialize)]
struct IgdbGame {
    id: i64,
    #[serde(default)]
    name: String,
    summary: Option<String>,
    storyline: Option<String>,
    first_release_date: Option<i64>,
    #[serde(default)]
    genres: Vec<IgdbNamed>,
    #[serde(default)]
    involved_companies: Vec<IgdbInvolved>,
    cover: Option<IgdbImage>,
    #[serde(default)]
    artworks: Vec<IgdbImage>,
    #[serde(default)]
    screenshots: Vec<IgdbImage>,
    #[serde(default)]
    external_games: Vec<IgdbExternal>,
}

#[derive(Debug, Deserialize)]
struct IgdbNamed {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct IgdbInvolved {
    #[serde(default)]
    developer: bool,
    #[serde(default)]
    publisher: bool,
    company: Option<IgdbNamed>,
}

#[derive(Debug, Deserialize)]
struct IgdbImage {
    #[serde(default)]
    image_id: String,
}

#[derive(Debug, Deserialize)]
struct IgdbExternal {
    category: Option<i64>,
    external_game_source: Option<i64>,
    uid: Option<String>,
}

fn igdb_image(id: &str, size: &str) -> Option<String> {
    (!id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric()))
        .then(|| format!("https://images.igdb.com/igdb/image/upload/{size}/{id}.jpg"))
}

impl IgdbGame {
    fn normalize(self) -> Option<Found> {
        if self.id <= 0 {
            return None;
        }
        let title = clamp(&self.name, 200)?;
        let summary_src = self
            .summary
            .as_deref()
            .map(html_to_text)
            .unwrap_or_default();
        let story = self
            .storyline
            .as_deref()
            .map(html_to_text)
            .unwrap_or_default();
        let description = [summary_src.as_str(), story.as_str()]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("\n\n");
        let date: Option<Date> = self
            .first_release_date
            .and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok())
            .map(OffsetDateTime::date);
        let company = |pick: fn(&IgdbInvolved) -> bool| {
            self.involved_companies
                .iter()
                .filter(|c| pick(c))
                .find_map(|c| c.company.as_ref().and_then(|n| clamp(&n.name, 200)))
        };
        let steam_app_id = self
            .external_games
            .iter()
            .filter(|e| {
                e.category == Some(IGDB_STEAM) || e.external_game_source == Some(IGDB_STEAM)
            })
            .find_map(|e| e.uid.as_deref()?.parse::<i64>().ok().filter(|&v| v > 0));
        let data = CandidateData {
            title: Some(title.clone()),
            summary: summarize(&summary_src, 500),
            description: clamp(&description, 20_000),
            release_date: date.map(iso_date),
            developer: company(|c| c.developer),
            publisher: company(|c| c.publisher),
            genres: genres(self.genres.into_iter().map(|g| g.name)),
            images: CandidateImages {
                cover: self
                    .cover
                    .as_ref()
                    .and_then(|c| igdb_image(&c.image_id, "t_cover_big_2x")),
                hero: self
                    .artworks
                    .first()
                    .and_then(|a| igdb_image(&a.image_id, "t_1080p")),
                logo: None,
                screenshots: self
                    .screenshots
                    .iter()
                    .filter_map(|s| igdb_image(&s.image_id, "t_1080p"))
                    .take(MAX_SCREENSHOTS)
                    .collect(),
            },
            external: CandidateExternal {
                steam_app_id,
                igdb_id: Some(self.id),
                umu_id: None,
            },
        };
        Some(Found {
            source: MetadataSource::Igdb,
            external_id: self.id,
            title,
            release_year: date.map(Date::year),
            data,
        })
    }
}

// -- Steam wire format --------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SteamDetailsEnvelope {
    #[serde(default)]
    success: bool,
    data: Option<SteamDetails>,
}

#[derive(Debug, Deserialize)]
struct SteamDetails {
    #[serde(default)]
    name: String,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    short_description: String,
    #[serde(default)]
    about_the_game: String,
    #[serde(default)]
    detailed_description: String,
    #[serde(default)]
    developers: Vec<String>,
    #[serde(default)]
    publishers: Vec<String>,
    #[serde(default)]
    genres: Vec<SteamGenre>,
    #[serde(default)]
    screenshots: Vec<SteamScreenshot>,
    release_date: Option<SteamRelease>,
}

#[derive(Debug, Deserialize)]
struct SteamGenre {
    #[serde(default)]
    description: String,
}

#[derive(Debug, Deserialize)]
struct SteamScreenshot {
    #[serde(default)]
    path_full: String,
}

#[derive(Debug, Deserialize)]
struct SteamRelease {
    #[serde(default)]
    date: String,
}

fn steam_asset(app_id: i64, file: &str) -> String {
    format!("https://shared.akamai.steamstatic.com/store_item_assets/steam/apps/{app_id}/{file}")
}

impl SteamDetails {
    fn normalize(self, app_id: i64) -> Option<Found> {
        if !self.kind.is_empty() && self.kind != "game" {
            return None;
        }
        let title = clamp(&self.name, 200)?;
        let long = if self.about_the_game.trim().is_empty() {
            &self.detailed_description
        } else {
            &self.about_the_game
        };
        let date = self
            .release_date
            .as_ref()
            .and_then(|r| parse_steam_date(&r.date));
        let data = CandidateData {
            title: Some(title.clone()),
            summary: summarize(&html_to_text(&self.short_description), 500),
            description: clamp(&html_to_text(long), 20_000),
            release_date: date.map(iso_date),
            developer: self.developers.first().and_then(|d| clamp(d, 200)),
            publisher: self.publishers.first().and_then(|p| clamp(p, 200)),
            genres: genres(self.genres.into_iter().map(|g| g.description)),
            images: CandidateImages {
                cover: Some(steam_asset(app_id, "library_600x900_2x.jpg")),
                hero: Some(steam_asset(app_id, "library_hero.jpg")),
                logo: Some(steam_asset(app_id, "logo.png")),
                // Anything off the allowlist is refused again at download time.
                screenshots: self
                    .screenshots
                    .into_iter()
                    .map(|s| s.path_full)
                    .filter(|u| u.starts_with("https://"))
                    .take(MAX_SCREENSHOTS)
                    .collect(),
            },
            external: CandidateExternal {
                steam_app_id: Some(app_id),
                igdb_id: None,
                umu_id: None,
            },
        };
        Some(Found {
            source: MetadataSource::Steam,
            external_id: app_id,
            title,
            release_year: date.map(Date::year),
            data,
        })
    }
}

fn iso_date(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

/// Shared handle stored in the application state.
pub type SharedProviders = Arc<Providers>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apicalypse_strings_cannot_break_out() {
        assert_eq!(
            apicalypse_string("a\"; fields *; where id = 1; \\"),
            "a fields * where id = 1 "
        );
    }

    #[test]
    fn umu_ids() {
        assert!(valid_umu_id("umu-504230"));
        assert!(!valid_umu_id("504230"));
        assert!(!valid_umu_id("umu-a/b"));
    }

    #[test]
    fn igdb_image_ids_are_checked() {
        assert!(igdb_image("co1tmu", "t_cover_big").is_some());
        assert!(igdb_image("../x", "t_cover_big").is_none());
        assert!(igdb_image("", "t_cover_big").is_none());
    }
}
