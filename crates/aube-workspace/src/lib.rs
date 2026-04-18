//! Workspace support for aube.
//!
//! Reads pnpm-workspace.yaml to discover workspace packages.
//! Supports the `workspace:` protocol for inter-package dependencies.

pub mod selector;

use std::path::{Path, PathBuf};

pub use aube_manifest::workspace::WorkspaceConfig;
pub use selector::{Selector, WorkspacePkg};

/// Discover workspace packages from pnpm-workspace.yaml.
pub fn find_workspace_packages(project_dir: &Path) -> Result<Vec<PathBuf>, Error> {
    let config = WorkspaceConfig::load(project_dir).map_err(|e| match e {
        aube_manifest::Error::Io(p, e) => Error::Io(p, e),
        aube_manifest::Error::YamlParse(p, e) => Error::Parse(p, e),
        _ => Error::Parse(project_dir.to_path_buf(), e.to_string()),
    })?;

    if config.packages.is_empty() {
        return Ok(vec![]);
    }

    let mut packages = Vec::new();
    for pattern in &config.packages {
        let full_pattern = project_dir.join(pattern).join("package.json");
        if let Ok(entries) = glob::glob(full_pattern.to_str().unwrap_or_default()) {
            for entry in entries.flatten() {
                if let Some(parent) = entry.parent() {
                    packages.push(parent.to_path_buf());
                }
            }
        }
    }

    Ok(packages)
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error at {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("failed to parse {0}: {1}")]
    Parse(PathBuf, String),
}
