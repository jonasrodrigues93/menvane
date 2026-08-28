use std::fmt::{Display, Formatter};
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::project::ProjectTechnologies;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KnowledgeType {
    Memory,
    Playbook,
}

impl KnowledgeType {
    pub fn directory_name(self) -> &'static str {
        match self {
            Self::Memory => "memories",
            Self::Playbook => "playbooks",
        }
    }
}

impl Display for KnowledgeType {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Memory => "memory",
            Self::Playbook => "playbook",
        })
    }
}

impl FromStr for KnowledgeType {
    type Err = ParseKnowledgeTypeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "memory" => Ok(Self::Memory),
            "playbook" => Ok(Self::Playbook),
            _ => Err(ParseKnowledgeTypeError(value.to_owned())),
        }
    }
}

#[derive(Debug, Error)]
#[error("unsupported knowledge type: {0}")]
pub struct ParseKnowledgeTypeError(String);

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryStatus {
    Active,
    Candidate,
    Quarantined,
    Superseded,
    Forgotten,
}

impl Display for MemoryStatus {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Active => "active",
            Self::Candidate => "candidate",
            Self::Quarantined => "quarantined",
            Self::Superseded => "superseded",
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

impl Applicability {
    pub fn is_empty(&self) -> bool {
        self.languages.is_empty()
            && self.frameworks.is_empty()
            && self.tools.is_empty()
            && self.databases.is_empty()
            && self.platforms.is_empty()
    }

    pub fn overlaps(&self, technologies: &ProjectTechnologies) -> bool {
        fn dimension_overlaps(values: &[String], detected: &[String]) -> bool {
            values.is_empty()
                || values
                    .iter()
                    .any(|value| detected.iter().any(|item| item.eq_ignore_ascii_case(value)))
        }
        dimension_overlaps(&self.languages, &technologies.languages)
            && dimension_overlaps(&self.frameworks, &technologies.frameworks)
            && dimension_overlaps(&self.tools, &technologies.tools)
            && dimension_overlaps(&self.databases, &technologies.databases)
            && dimension_overlaps(&self.platforms, &technologies.platforms)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeMetadata {
    pub id: Uuid,
    #[serde(rename = "type")]
    pub knowledge_type: KnowledgeType,
    pub scope: Scope,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub status: MemoryStatus,
    #[serde(
        default = "default_relevance",
        skip_serializing_if = "is_default_relevance"
    )]
    pub confidence: f64,
    #[serde(
        default = "default_relevance",
        skip_serializing_if = "is_default_relevance"
    )]
    pub utility: f64,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_project_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decayed_at: Option<DateTime<Utc>>,
}

impl KnowledgeMetadata {
    pub fn new(
        knowledge_type: KnowledgeType,
        scope: Scope,
        project_id: Option<String>,
        tags: Vec<String>,
        applies_to: Applicability,
        status: MemoryStatus,
    ) -> Self {
        let now = Utc::now();
        let playbook = knowledge_type == KnowledgeType::Playbook;
        Self {
            id: Uuid::now_v7(),
            knowledge_type,
            scope,
            project_id,
            status,
            confidence: if status == MemoryStatus::Candidate {
                0.6
            } else {
                1.0
            },
            utility: 1.0,
            created_at: now,
            updated_at: now,
            last_verified_at: Some(now),
            source_sessions: Vec::new(),
            tags,
            applies_to,
            supersedes: Vec::new(),
            successes: playbook.then_some(0),
            failures: playbook.then_some(0),
            source_project_ids: Vec::new(),
            decayed_at: None,
        }
    }
}

fn default_relevance() -> f64 {
    1.0
}

fn is_default_relevance(value: &f64) -> bool {
    (*value - 1.0).abs() < f64::EPSILON
}

#[derive(Debug, Clone, PartialEq)]
pub struct KnowledgeRecord {
    pub metadata: KnowledgeMetadata,
    pub title: String,
    pub body: String,
}
