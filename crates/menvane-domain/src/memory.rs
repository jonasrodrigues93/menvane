use std::fmt::{Display, Formatter};
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryType {
    Fact,
    Decision,
    Procedure,
    Gotcha,
    Session,
}

impl MemoryType {
    pub fn directory_name(self) -> &'static str {
        match self {
            Self::Fact => "facts",
            Self::Decision => "decisions",
            Self::Procedure => "procedures",
            Self::Gotcha => "gotchas",
            Self::Session => "sessions",
        }
    }
}

impl Display for MemoryType {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Fact => "fact",
            Self::Decision => "decision",
            Self::Procedure => "procedure",
            Self::Gotcha => "gotcha",
            Self::Session => "session",
        })
    }
}

impl FromStr for MemoryType {
    type Err = ParseMemoryTypeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "fact" => Ok(Self::Fact),
            "decision" => Ok(Self::Decision),
            "procedure" => Ok(Self::Procedure),
            "gotcha" => Ok(Self::Gotcha),
            "session" => Ok(Self::Session),
            _ => Err(ParseMemoryTypeError(value.to_owned())),
        }
    }
}

#[derive(Debug, Error)]
#[error("unsupported memory type: {0}")]
pub struct ParseMemoryTypeError(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    Global,
    Project,
}

impl Display for Scope {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Global => "global",
            Self::Project => "project",
        })
    }
}

impl FromStr for Scope {
    type Err = ParseScopeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "global" => Ok(Self::Global),
            "project" => Ok(Self::Project),
            _ => Err(ParseScopeError(value.to_owned())),
        }
    }
}

#[derive(Debug, Error)]
#[error("unsupported scope: {0}")]
pub struct ParseScopeError(String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryStatus {
    Active,
    Candidate,
    NeedsValidation,
    Superseded,
    Historical,
    Forgotten,
}

impl Display for MemoryStatus {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Active => "active",
            Self::Candidate => "candidate",
            Self::NeedsValidation => "needs-validation",
            Self::Superseded => "superseded",
            Self::Historical => "historical",
            Self::Forgotten => "forgotten",
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applicability {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub languages: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frameworks: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub databases: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryMetadata {
    pub id: Uuid,
    #[serde(rename = "type")]
    pub memory_type: MemoryType,
    pub scope: Scope,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub status: MemoryStatus,
    pub confidence: f64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_verified_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_sessions: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Applicability::is_empty")]
    pub applies_to: Applicability,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub successes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failures: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u32>,
}

impl Applicability {
    pub fn is_empty(&self) -> bool {
        self.languages.is_empty()
            && self.frameworks.is_empty()
            && self.tools.is_empty()
            && self.databases.is_empty()
            && self.platforms.is_empty()
    }
}

impl MemoryMetadata {
    pub fn new(
        memory_type: MemoryType,
        scope: Scope,
        project_id: Option<String>,
        confidence: f64,
        tags: Vec<String>,
        applies_to: Applicability,
    ) -> Self {
        let now = Utc::now();
        let procedure = memory_type == MemoryType::Procedure;
        Self {
            id: Uuid::now_v7(),
            memory_type,
            scope,
            project_id,
            status: if procedure {
                MemoryStatus::Candidate
            } else {
                MemoryStatus::Active
            },
            confidence,
            created_at: now,
            updated_at: now,
            last_verified_at: Some(now),
            source_sessions: Vec::new(),
            tags,
            applies_to,
            supersedes: Vec::new(),
            successes: procedure.then_some(1),
            failures: procedure.then_some(0),
            client: None,
            external_session_id: None,
            started_at: None,
            ended_at: None,
            imported: None,
            generation: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Memory {
    pub metadata: MemoryMetadata,
    pub title: String,
    pub body: String,
}
