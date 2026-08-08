use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectTechnologies {
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub frameworks: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub databases: Vec<String>,
    #[serde(default)]
    pub platforms: Vec<String>,
}

impl ProjectTechnologies {
    pub fn normalize(&mut self) {
        for values in [
            &mut self.languages,
            &mut self.frameworks,
            &mut self.tools,
            &mut self.databases,
            &mut self.platforms,
        ] {
            values.sort_unstable();
            values.dedup();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub identity: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub known_paths: Vec<String>,
    #[serde(default)]
    pub technologies: ProjectTechnologies,
}
