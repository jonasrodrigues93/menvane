use std::path::PathBuf;

use menvane_domain::{KnowledgeType, MemoryStatus, Scope};
use menvane_store::MarkdownStore;

#[test]
fn parses_canonical_context_fixture() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/context.md");
    let store = MarkdownStore::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    let memory = store.parse_memory(&fixture).unwrap();
    assert_eq!(memory.metadata.knowledge_type, KnowledgeType::Context);
    assert_eq!(memory.metadata.scope, Scope::Global);
    assert_eq!(memory.metadata.status, MemoryStatus::Active);
    assert_eq!(memory.metadata.applies_to.databases, ["sqlite"]);
    assert_eq!(memory.title, "Markdown remains durable");
    assert_eq!(memory.body, "SQLite can be rebuilt from Markdown.");
}
