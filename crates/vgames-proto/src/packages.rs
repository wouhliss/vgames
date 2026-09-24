//! Packages, assets, the public catalog and admin package management.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::{auth::UserPublic, jobs::Job};

/// Build targets (docs/architecture/02-package-format.md §5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum Platform {
    #[serde(rename = "windows-x86_64")]
    WindowsX86_64,
    #[serde(rename = "windows-aarch64")]
    WindowsAarch64,
    #[serde(rename = "linux-x86_64")]
    LinuxX86_64,
    #[serde(rename = "linux-aarch64")]
    LinuxAarch64,
    #[serde(rename = "macos-aarch64")]
    MacosAarch64,
    #[serde(rename = "macos-x86_64")]
    MacosX86_64,
}

impl Platform {
    pub const ALL: [Platform; 6] = [
        Platform::WindowsX86_64,
        Platform::WindowsAarch64,
        Platform::LinuxX86_64,
        Platform::LinuxAarch64,
        Platform::MacosAarch64,
        Platform::MacosX86_64,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Platform::WindowsX86_64 => "windows-x86_64",
            Platform::WindowsAarch64 => "windows-aarch64",
            Platform::LinuxX86_64 => "linux-x86_64",
            Platform::LinuxAarch64 => "linux-aarch64",
            Platform::MacosAarch64 => "macos-aarch64",
            Platform::MacosX86_64 => "macos-x86_64",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum PackageStatus {
    Draft,
    Published,
    Hidden,
    Archived,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Cover,
    Hero,
    Logo,
    Screenshot,
    Icon,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ImageType {
    #[serde(rename = "image/jpeg")]
    Jpeg,
    #[serde(rename = "image/png")]
    Png,
    #[serde(rename = "image/webp")]
    Webp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum AssetSource {
    Igdb,
    Steam,
    Upload,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum FieldSource {
    Admin,
    Igdb,
    Steam,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ProtonDbTier {
    Platinum,
    Gold,
    Silver,
    Bronze,
    Borked,
    Pending,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Asset {
    pub id: Uuid,
    pub kind: AssetKind,
    /// API path `/v1/assets/{id}` (redirects to a signed URL).
    pub url: String,
    pub width: i32,
    pub height: i32,
    pub content_type: ImageType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<AssetSource>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PackageSummary {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<Asset>,
    pub platforms: Vec<Platform>,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReleaseInfo {
    pub platform: Platform,
    pub version_id: Uuid,
    pub version_label: String,
    pub sequence: i64,
    pub total_size: i64,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub published_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PackageDetail {
    #[serde(flatten)]
    pub summary: PackageSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub developer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "iso_date")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>, format = Date))]
    pub release_date: Option<Date>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protondb_tier: Option<ProtonDbTier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hero: Option<Asset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo: Option<Asset>,
    #[serde(default)]
    pub screenshots: Vec<Asset>,
    #[serde(default)]
    pub releases: Vec<ReleaseInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AdminPackage {
    #[serde(flatten)]
    pub detail: PackageDetail,
    pub status: PackageStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steam_app_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub igdb_id: Option<i64>,
    pub field_sources: BTreeMap<String, FieldSource>,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
    pub created_by: UserPublic,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_job: Option<Job>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PackagePage {
    pub items: Vec<PackageSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AdminPackagePage {
    pub items: Vec<AdminPackage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct AdminPackageCreate {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steam_app_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub igdb_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_metadata: Option<bool>,
}

/// Distinguishes "absent" (`None`) from "set to null" (`Some(None)`) in merge patches.
fn double_option<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(de).map(Some)
}

/// Calendar dates as `YYYY-MM-DD` (`time`'s own serde format is a `[year, ordinal]` tuple).
mod iso_date {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};
    use time::{Date, format_description::BorrowedFormatItem, macros::format_description};

    const FORMAT: &[BorrowedFormatItem<'static>] = format_description!("[year]-[month]-[day]");

    fn parse<E: Error>(s: &str) -> Result<Date, E> {
        Date::parse(s, FORMAT).map_err(|_| E::custom("expected a date as YYYY-MM-DD"))
    }

    fn write<S: Serializer>(d: &Date, s: S) -> Result<S::Ok, S::Error> {
        let text = d.format(FORMAT).map_err(serde::ser::Error::custom)?;
        s.serialize_str(&text)
    }

    pub fn serialize<S: Serializer>(d: &Option<Date>, s: S) -> Result<S::Ok, S::Error> {
        match d {
            Some(d) => write(d, s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Option<Date>, D::Error> {
        Option::<String>::deserialize(de)?
            .as_deref()
            .map(parse)
            .transpose()
    }

    /// Merge-patch form: absent → `None`, `null` → `Some(None)`.
    pub mod patch {
        use super::*;

        pub fn serialize<S: Serializer>(d: &Option<Option<Date>>, s: S) -> Result<S::Ok, S::Error> {
            match d {
                Some(Some(d)) => write(d, s),
                _ => s.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            de: D,
        ) -> Result<Option<Option<Date>>, D::Error> {
            super::deserialize(de).map(Some)
        }
    }
}

/// JSON Merge Patch. Every field set here is marked with source `admin`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct AdminPackagePatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub summary: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub description: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub developer: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub publisher: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "iso_date::patch"
    )]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>, format = Date))]
    pub release_date: Option<Option<Date>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genres: Option<Vec<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub steam_app_id: Option<Option<i64>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub igdb_id: Option<Option<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<PackageStatus>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub cover_asset_id: Option<Option<Uuid>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub hero_asset_id: Option<Option<Uuid>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub logo_asset_id: Option<Option<Uuid>>,
}

// ---------------------------------------------------------------------------------------
// Metadata candidates (docs/architecture/03-api.md §5)
// ---------------------------------------------------------------------------------------

/// A metadata provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum MetadataSource {
    Igdb,
    Steam,
}

impl MetadataSource {
    pub fn as_str(self) -> &'static str {
        match self {
            MetadataSource::Igdb => "igdb",
            MetadataSource::Steam => "steam",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "igdb" => Some(MetadataSource::Igdb),
            "steam" => Some(MetadataSource::Steam),
            _ => None,
        }
    }
}

/// Image URLs a provider offers (downloaded server-side only when applied).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CandidateImages {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(format = "uri"))]
    pub cover: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(format = "uri"))]
    pub hero: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(format = "uri"))]
    pub logo: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(schema_with = uri_array))]
    pub screenshots: Vec<String>,
}

/// `type: array, items: {type: string, format: uri}`.
#[cfg(feature = "openapi")]
fn uri_array() -> utoipa::openapi::schema::Array {
    use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder, SchemaFormat, Type};
    ArrayBuilder::new()
        .items(
            ObjectBuilder::new()
                .schema_type(Type::String)
                .format(Some(SchemaFormat::Custom("uri".into()))),
        )
        .build()
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CandidateExternal {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steam_app_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub igdb_id: Option<i64>,
    /// umu-database id for Proton compatibility fixes (a hint for compat profiles; Steam only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub umu_id: Option<String>,
}

/// Normalized provider data: plain text, clamped to the package column limits.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CandidateData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `YYYY-MM-DD`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(format = Date))]
    pub release_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub developer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub genres: Vec<String>,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(inline))]
    pub images: CandidateImages,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(inline))]
    pub external: CandidateExternal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MetadataCandidate {
    #[cfg_attr(feature = "openapi", schema(inline))]
    pub source: MetadataSource,
    #[cfg_attr(feature = "openapi", schema(minimum = 1))]
    pub external_id: i64,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_year: Option<i32>,
    /// Normalized-title similarity with the package title, 0..1.
    #[cfg_attr(feature = "openapi", schema(minimum = 0, maximum = 1))]
    pub score: f32,
    #[cfg_attr(feature = "openapi", schema(inline))]
    pub data: CandidateData,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub fetched_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MetadataCandidateList {
    pub items: Vec<MetadataCandidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<Job>,
}

/// Package fields a candidate can fill.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum MetadataField {
    Title,
    Summary,
    Description,
    ReleaseDate,
    Developer,
    Publisher,
    Genres,
    Cover,
    Hero,
    Logo,
    Screenshots,
    ExternalIds,
}

impl MetadataField {
    pub const ALL: [MetadataField; 12] = [
        MetadataField::Title,
        MetadataField::Summary,
        MetadataField::Description,
        MetadataField::ReleaseDate,
        MetadataField::Developer,
        MetadataField::Publisher,
        MetadataField::Genres,
        MetadataField::Cover,
        MetadataField::Hero,
        MetadataField::Logo,
        MetadataField::Screenshots,
        MetadataField::ExternalIds,
    ];
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct MetadataApply {
    #[cfg_attr(feature = "openapi", schema(inline))]
    pub source: MetadataSource,
    #[cfg_attr(feature = "openapi", schema(minimum = 1))]
    pub external_id: i64,
    #[cfg_attr(feature = "openapi", schema(inline, min_items = 1))]
    pub fields: std::collections::BTreeSet<MetadataField>,
    #[serde(default)]
    pub overwrite_admin_fields: bool,
}
