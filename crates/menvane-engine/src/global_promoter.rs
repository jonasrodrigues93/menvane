use std::collections::HashMap;

use anyhow::Result;
use chrono::Utc;
use menvane_domain::{Memory, MemoryStatus, MemoryType, Scope};
use uuid::Uuid;

use crate::{Menvane, normalize_markdown};

pub struct GlobalPromoter<'a> {
    menvane: &'a Menvane,
}

impl<'a> GlobalPromoter<'a> {
    pub fn new(menvane: &'a Menvane) -> Self {
        Self { menvane }
    }

    pub fn promote(&self) -> Result<Vec<Uuid>> {
        let mut groups: HashMap<String, Vec<(Memory, std::path::PathBuf)>> = HashMap::new();
        for path in self.menvane.markdown.memory_files()? {
            let memory = self.menvane.markdown.parse_memory(&path)?;
            if memory.metadata.memory_type != MemoryType::Procedure
                && memory.metadata.memory_type != MemoryType::Gotcha
            {
                continue;
            }
            if memory.metadata.scope != Scope::Project
                || matches!(
                    memory.metadata.status,
                    MemoryStatus::Forgotten | MemoryStatus::Superseded | MemoryStatus::Historical
                )
            {
                continue;
            }
            let key = format!(
                "{}:{}:{}",
                memory.metadata.memory_type,
                memory.title.to_ascii_lowercase(),
                normalize_markdown(&memory.body)
            );
            groups.entry(key).or_default().push((memory, path));
        }
        let mut promoted = Vec::new();
        for variants in groups.into_values() {
            let project_ids = variants
                .iter()
                .filter_map(|(memory, _)| memory.metadata.project_id.clone())
                .collect::<std::collections::HashSet<_>>();
            if project_ids.len() < 2 {
                continue;
            }
            let (first, _) = &variants[0];
            let global_exists = self
                .menvane
                .search(
                    std::path::Path::new("."),
                    &first.title,
                    crate::ScopeSelection::Global,
                    20,
                )?
                .iter()
                .any(|memory| {
                    memory.memory_type == first.metadata.memory_type.to_string()
                        && memory.title.eq_ignore_ascii_case(&first.title)
                });
            if global_exists {
                continue;
            }
            let mut global = first.clone();
            global.metadata.id = Uuid::now_v7();
            global.metadata.scope = Scope::Global;
            global.metadata.project_id = None;
            global.metadata.status = MemoryStatus::Active;
            global.metadata.created_at = Utc::now();
            global.metadata.updated_at = Utc::now();
            global.metadata.source_project_ids = project_ids.into_iter().collect();
            global.metadata.source_project_ids.sort();
            global.metadata.source_sessions = variants
                .iter()
                .flat_map(|(memory, _)| memory.metadata.source_sessions.iter().copied())
                .collect();
            global.metadata.source_sessions.sort();
            global.metadata.source_sessions.dedup();
            global.metadata.successes = Some(
                variants
                    .iter()
                    .map(|(memory, _)| memory.metadata.successes.unwrap_or(0))
                    .sum(),
            );
            global.metadata.failures = Some(
                variants
                    .iter()
                    .map(|(memory, _)| memory.metadata.failures.unwrap_or(0))
                    .sum(),
            );
            let path = self.menvane.markdown.write_memory(&global, None)?;
            self.menvane.index.upsert_memory(&global, &path)?;
            for (mut variant, path) in variants {
                variant.metadata.status = MemoryStatus::Historical;
                variant.metadata.updated_at = Utc::now();
                self.menvane.markdown.update_memory(&path, &variant)?;
                self.menvane.index.upsert_memory(&variant, &path)?;
            }
            self.menvane
                .markdown
                .commit(&format!("feat(memory): promote {}", global.metadata.id));
            promoted.push(global.metadata.id);
        }
        Ok(promoted)
    }
}
