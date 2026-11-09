//! The files the engine hands out: every target's generated files, and the project's own (the
//! appliances' configurations and the starter scripts of its provisioning steps).

use isoloom_targets::GeneratedFile;
use isothrum_compiler::ProjectFile;
use serde::Serialize;

use crate::plan::compiled;

/// A file: its path and what it holds.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct File {
    /// Its path, relative to the project's folder.
    pub path: String,
    /// What it holds.
    pub contents: String,
}

impl From<GeneratedFile> for File {
    fn from(f: GeneratedFile) -> File {
        File {
            path: f.path,
            contents: f.contents,
        }
    }
}

impl From<ProjectFile> for File {
    fn from(f: ProjectFile) -> File {
        File {
            path: f.path,
            contents: f.contents,
        }
    }
}

/// The project's own files, next to isoloom.yml where its paths point: the appliances'
/// configurations and the starter scripts of the machines' provisioning steps. What a program
/// writes with the spec to run it; none for a topology that doesn't compile.
#[must_use]
pub(crate) fn own_files(topology_json: &str) -> Vec<File> {
    compiled(topology_json)
        .map(|(_, c)| c.into_parts().2.into_iter().map(File::from).collect())
        .unwrap_or_default()
}
