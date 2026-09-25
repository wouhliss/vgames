//! Background job status as shown to admins.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Dead,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Job {
    pub id: Uuid,
    pub kind: String,
    pub state: JobState,
    pub attempts: i32,
    pub max_attempts: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>, format = DateTime))]
    pub run_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>, format = DateTime))]
    pub finished_at: Option<OffsetDateTime>,
}

impl JobState {
    pub const ALL: [JobState; 5] = [
        JobState::Queued,
        JobState::Running,
        JobState::Succeeded,
        JobState::Failed,
        JobState::Dead,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Succeeded => "succeeded",
            JobState::Failed => "failed",
            JobState::Dead => "dead",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|j| j.as_str() == s)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct JobPage {
    pub items: Vec<Job>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}
