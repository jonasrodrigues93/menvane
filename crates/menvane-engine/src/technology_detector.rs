use std::fs;
use std::path::Path;

use anyhow::Result;
use menvane_domain::ProjectTechnologies;
use serde_json::Value;

pub struct TechnologyDetector;

impl TechnologyDetector {
    pub fn detect(root: &Path) -> Result<ProjectTechnologies> {
        let mut technologies = ProjectTechnologies::default();
        detect_known_files(root, &mut technologies);
        detect_package_json(root, &mut technologies)?;
        detect_text_dependencies(root, &mut technologies)?;
        technologies.normalize();
        Ok(technologies)
    }
}

fn detect_known_files(root: &Path, technologies: &mut ProjectTechnologies) {
    let files = [
        ("Cargo.toml", "rust", "cargo"),
        ("package.json", "javascript", "npm"),
        ("pom.xml", "java", "maven"),
        ("build.gradle", "java", "gradle"),
        ("build.gradle.kts", "kotlin", "gradle"),
        ("pyproject.toml", "python", "python"),
        ("requirements.txt", "python", "pip"),
        ("go.mod", "go", "go"),
        ("composer.json", "php", "composer"),
        ("Gemfile", "ruby", "bundler"),
    ];
    for (file, language, tool) in files {
        if root.join(file).exists() {
            technologies.languages.push(language.to_owned());
            technologies.tools.push(tool.to_owned());
        }
    }
    if root.join("pnpm-lock.yaml").exists() {
        technologies.tools.push("pnpm".to_owned());
    }
    if root.join("yarn.lock").exists() {
        technologies.tools.push("yarn".to_owned());
    }
    if root.join("tsconfig.json").exists() {
        technologies.languages.push("typescript".to_owned());
    }
    if root.join("Dockerfile").exists()
        || root.join("docker-compose.yml").exists()
        || root.join("docker-compose.yaml").exists()
        || root.join("compose.yml").exists()
        || root.join("compose.yaml").exists()
    {
        technologies.platforms.push("docker".to_owned());
        technologies.tools.push("docker".to_owned());
    }
}

fn detect_package_json(root: &Path, technologies: &mut ProjectTechnologies) -> Result<()> {
    let path = root.join("package.json");
    if !path.exists() {
        return Ok(());
    }
    let package: Value = serde_json::from_str(&fs::read_to_string(path)?)?;
    for section in ["dependencies", "devDependencies"] {
        let dependencies = package
            .get(section)
            .and_then(Value::as_object)
            .into_iter()
            .flatten();
        for dependency in dependencies.map(|(name, _)| name.as_str()) {
            match dependency {
                "typescript" => technologies.languages.push("typescript".to_owned()),
                "react" | "react-dom" => technologies.frameworks.push("react".to_owned()),
                "vue" => technologies.frameworks.push("vue".to_owned()),
                "svelte" => technologies.frameworks.push("svelte".to_owned()),
                "next" => technologies.frameworks.push("next.js".to_owned()),
                "express" => technologies.frameworks.push("express".to_owned()),
                "@nestjs/core" => technologies.frameworks.push("nestjs".to_owned()),
                "prisma" | "@prisma/client" => technologies.tools.push("prisma".to_owned()),
                "pg" => technologies.databases.push("postgresql".to_owned()),
                "mysql" | "mysql2" => technologies.databases.push("mysql".to_owned()),
                "sqlite3" | "better-sqlite3" => technologies.databases.push("sqlite".to_owned()),
                "redis" | "ioredis" => technologies.databases.push("redis".to_owned()),
                _ => {}
            }
        }
    }
    Ok(())
}

fn detect_text_dependencies(root: &Path, technologies: &mut ProjectTechnologies) -> Result<()> {
    for file in [
        "Cargo.toml",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "pyproject.toml",
        "requirements.txt",
        "go.mod",
    ] {
        let path = root.join(file);
        if !path.exists() {
            continue;
        }
        let content = fs::read_to_string(path)?.to_ascii_lowercase();
        for (needle, dimension, value) in [
            ("axum", "framework", "axum"),
            ("spring", "framework", "spring"),
            ("django", "framework", "django"),
            ("fastapi", "framework", "fastapi"),
            ("flask", "framework", "flask"),
            ("postgres", "database", "postgresql"),
            ("mysql", "database", "mysql"),
            ("sqlite", "database", "sqlite"),
            ("mongodb", "database", "mongodb"),
        ] {
            if content.contains(needle) {
                match dimension {
                    "framework" => technologies.frameworks.push(value.to_owned()),
                    "database" => technologies.databases.push(value.to_owned()),
                    _ => unreachable!(),
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn detects_rust_axum_and_sqlite() {
        let temporary = TempDir::new().unwrap();
        fs::write(
            temporary.path().join("Cargo.toml"),
            "[dependencies]\naxum = \"1\"\nrusqlite = \"1\"\n",
        )
        .unwrap();
        let detected = TechnologyDetector::detect(temporary.path()).unwrap();
        assert_eq!(detected.languages, ["rust"]);
        assert_eq!(detected.frameworks, ["axum"]);
        assert_eq!(detected.databases, ["sqlite"]);
        assert_eq!(detected.tools, ["cargo"]);
    }
}
