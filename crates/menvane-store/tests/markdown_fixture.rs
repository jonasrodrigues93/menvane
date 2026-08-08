use std::path::PathBuf;

use menvane_domain::{MemoryStatus, MemoryType, Scope};
use menvane_store::MarkdownStore;

#[test]
fn parses_canonical_fact_fixture() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fact.md");
    let store = MarkdownStore::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    let memory = store.parse_memory(&fixture).unwrap();
    assert_eq!(memory.metadata.memory_type, MemoryType::Fact);
    assert_eq!(memory.metadata.scope, Scope::Global);
    assert_eq!(memory.metadata.status, MemoryStatus::Active);
    assert_eq!(memory.metadata.applies_to.databases, ["sqlite"]);
    assert_eq!(memory.title, "Markdown remains durable");
    assert_eq!(memory.body, "SQLite can be rebuilt from Markdown.");
}
