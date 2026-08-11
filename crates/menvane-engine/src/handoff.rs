use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::CaptureSanitizer;

use menvane_store::{EpisodeEvent, MAX_HANDOFF_CHANGED_FILES, MAX_HANDOFF_ITEM_BYTES};

#[derive(Debug, Clone)]
pub struct RepositoryState {
    pub changed_files: Vec<String>,
    pub git_head: Option<String>,
    pub worktree_state_hash: Option<String>,
}

pub fn repository_state(
    cwd: &Path,
    events: &[EpisodeEvent],
    sanitizer: &CaptureSanitizer,
) -> RepositoryState {
    let cwd_text = cwd.to_string_lossy().into_owned();
    let status = Command::new("git")
        .args([
            "-C",
            &cwd_text,
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ])
        .output();
    let Some(status) = status.ok().filter(|value| value.status.success()) else {
        return RepositoryState {
            changed_files: attributed_files(events, sanitizer),
            git_head: None,
            worktree_state_hash: None,
        };
    };
    let changed_files = changed_files_from_git_status(&status.stdout, sanitizer);
    let git_head = Command::new("git")
        .args(["-C", &cwd_text, "rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|value| value.status.success())
        .map(|value| String::from_utf8_lossy(&value.stdout).trim().to_owned())
        .filter(|value| !value.is_empty());
    let mut hasher = Sha256::new();
    hasher.update(&status.stdout);
    let worktree_state_hash = Some(hex::encode(hasher.finalize()));
    RepositoryState {
        changed_files,
        git_head,
        worktree_state_hash,
    }
}

fn attributed_files(events: &[EpisodeEvent], sanitizer: &CaptureSanitizer) -> Vec<String> {
    events
        .iter()
        .filter_map(|value| value.event.attributed_path.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter(|value| !sanitizer.path_is_ignored(value))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|value| bounded(&value, MAX_HANDOFF_ITEM_BYTES))
        .take(MAX_HANDOFF_CHANGED_FILES)
        .collect()
}

fn changed_files_from_git_status(status: &[u8], sanitizer: &CaptureSanitizer) -> Vec<String> {
    let records = status
        .split(|value| *value == 0)
        .filter(|record| !record.is_empty())
        .collect::<Vec<_>>();
    let mut paths = BTreeSet::new();
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        if record.len() < 4 || record[2] != b' ' {
            index += 1;
            continue;
        }
        let path = String::from_utf8_lossy(&record[3..]).into_owned();
        if !sanitizer.path_is_ignored(&path) {
            paths.insert(path);
        }
        let renamed = matches!(record[0], b'R' | b'C') || matches!(record[1], b'R' | b'C');
        if renamed {
            if let Some(previous) = records.get(index + 1) {
                let path = String::from_utf8_lossy(previous).into_owned();
                if !sanitizer.path_is_ignored(&path) {
                    paths.insert(path);
                }
            }
            index += 2;
        } else {
            index += 1;
        }
    }
    paths
        .into_iter()
        .map(|value| bounded(&value, MAX_HANDOFF_ITEM_BYTES))
        .take(MAX_HANDOFF_CHANGED_FILES)
        .collect()
}

fn bounded(value: &str, limit: usize) -> String {
    let value = value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(|line| {
            let lowercase = line.to_ascii_lowercase();
            [
                "authorization:",
                "api_key=",
                "api-key=",
                "access_token=",
                "access-token=",
                "password=",
                "secret=",
            ]
            .iter()
            .find_map(|marker| {
                lowercase
                    .find(marker)
                    .map(|index| format!("{}[REDACTED]", &line[..index + marker.len()]))
            })
            .unwrap_or_else(|| line.to_owned())
        })
        .collect::<Vec<_>>()
        .join("\n");
    truncate_utf8(&value, limit)
}

fn truncate_utf8(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::{changed_files_from_git_status, truncate_utf8};
    use crate::CaptureSanitizer;

    #[test]
    fn parses_nul_delimited_changes_and_renames() {
        let status = b" M src/current.rs\0R  src/new.rs\0src/old.rs\0?? odd name.rs\0";
        let sanitizer = CaptureSanitizer::new(Default::default()).unwrap();
        assert_eq!(
            changed_files_from_git_status(status, &sanitizer),
            vec![
                "odd name.rs".to_owned(),
                "src/current.rs".to_owned(),
                "src/new.rs".to_owned(),
                "src/old.rs".to_owned(),
            ]
        );
    }

    #[test]
    fn excludes_instruction_files_from_changed_files() {
        let status = b" M AGENTS.md\0 M src/lib.rs\0?? skills/custom/SKILL.md\0";
        let sanitizer = CaptureSanitizer::new(Default::default()).unwrap();
        assert_eq!(
            changed_files_from_git_status(status, &sanitizer),
            vec!["src/lib.rs".to_owned()]
        );
    }

    #[test]
    fn truncates_on_utf8_byte_boundaries() {
        assert_eq!(truncate_utf8("ééé", 5), "éé");
        assert_eq!(truncate_utf8("ééé", 4), "éé");
        assert_eq!(truncate_utf8("ééé", 3), "é");
    }
}
