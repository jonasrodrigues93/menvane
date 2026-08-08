use std::fs;

use menvane_domain::{Applicability, MemoryType, Scope};
use menvane_engine::{Menvane, ScopeSelection, WriteMemory};
use tempfile::TempDir;

#[test]
fn automatic_recall_filters_global_applicability() {
    let temporary = TempDir::new().unwrap();
    let java = temporary.path().join("java-project");
    let python = temporary.path().join("python-project");
    fs::create_dir_all(&java).unwrap();
    fs::create_dir_all(&python).unwrap();
    fs::write(
        java.join("pom.xml"),
        "<project><artifactId>app</artifactId></project>",
    )
    .unwrap();
    fs::write(python.join("pyproject.toml"), "[project]\nname = \"app\"\n").unwrap();
    let menvane = Menvane::new(temporary.path().join("home")).unwrap();

    write_global(
        &menvane,
        &java,
        "Universal build guidance",
        "build-guidance universal",
        Applicability::default(),
    );
    write_global(
        &menvane,
        &java,
        "Maven build guidance",
        "build-guidance maven",
        Applicability {
            languages: vec!["java".to_owned()],
            tools: vec!["maven".to_owned()],
            ..Applicability::default()
        },
    );
    write_global(
        &menvane,
        &python,
        "Python build guidance",
        "build-guidance python",
        Applicability {
            languages: vec!["python".to_owned()],
            ..Applicability::default()
        },
    );

    let java_results = menvane.recall(&java, "build-guidance", 10).unwrap();
    assert_eq!(
        titles(&java_results),
        ["Maven build guidance", "Universal build guidance"]
    );
    let python_results = menvane.recall(&python, "build-guidance", 10).unwrap();
    assert_eq!(
        titles(&python_results),
        ["Python build guidance", "Universal build guidance"]
    );
    let explicit = menvane
        .search(&python, "maven build-guidance", ScopeSelection::Auto, 10)
        .unwrap();
    assert_eq!(titles(&explicit), ["Maven build guidance"]);
}

fn write_global(
    menvane: &Menvane,
    cwd: &std::path::Path,
    title: &str,
    body: &str,
    applies_to: Applicability,
) {
    menvane
        .write(
            cwd,
            WriteMemory {
                title: title.to_owned(),
                body: body.to_owned(),
                memory_type: MemoryType::Fact,
                scope: Scope::Global,
                confidence: 1.0,
                tags: Vec::new(),
                applies_to,
            },
        )
        .unwrap();
}

fn titles(results: &[menvane_store::SearchResult]) -> Vec<&str> {
    let mut titles = results
        .iter()
        .map(|result| result.title.as_str())
        .collect::<Vec<_>>();
    titles.sort_unstable();
    titles
}
