use std::path::Path;
use std::process::Command;

pub fn init_git(path: &Path) {
    let status = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["init", "--quiet"])
        .status()
        .unwrap();
    assert!(status.success());
}
