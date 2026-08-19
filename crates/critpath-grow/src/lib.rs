//! Performance findings read from source, without building or running anything.
//!
//! The engine reads files, folds them into blocks under a [`grammar`] table, and solves how much
//! each routine's work grows with a dynamic program over the call graph. What it reports is a
//! position and an exponent: the line that carries the growth, and how many nested repetitions it
//! sits under. No time is claimed, because a source file contains none.

#![forbid(unsafe_code)]

pub mod degree;
pub mod grammar;
pub mod rules;
pub mod tree;

use std::path::Path;

pub use degree::{At, Growth, Solved};
pub use rules::{check, Rule, Spot};
pub use tree::File;

/// Everything read from a root.
#[derive(Clone, Debug, Default)]
pub struct Sources {
    /// The files, in path order.
    pub files: Vec<File>,
    /// Files whose extension no grammar claims.
    pub unread: usize,
}

impl Sources {
    /// Solve the growth of every routine read.
    #[must_use]
    pub fn solve(&self) -> Solved {
        degree::solve(&self.files)
    }

    /// The path of a file by index.
    #[must_use]
    pub fn path(&self, index: usize) -> &str {
        self.files.get(index).map_or("", |file| file.path.as_str())
    }
}

/// Directories that hold installed rather than authored code.
///
/// Not descended into: what is in them is not the repository's to change, and reading them would
/// bury the lines that are.
const INSTALLED: &[&str] =
    &["node_modules", ".git", "target", "dist", "build", "out", "vendor", ".next", "coverage"];

/// Read every file under `root` that a grammar claims.
#[must_use]
pub fn read(root: &Path) -> Sources {
    let mut sources = Sources::default();
    let mut queue = vec![root.to_path_buf()];
    while let Some(directory) = queue.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if !INSTALLED.contains(&name.as_str()) && !name.starts_with('.') {
                    queue.push(path);
                }
                continue;
            }
            let Some(extension) = path.extension().map(|e| e.to_string_lossy().to_string()) else {
                continue;
            };
            let Some(grammar) = grammar::for_extension(&extension) else {
                sources.unread += 1;
                continue;
            };
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let relative =
                path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            sources.files.push(tree::read(&relative, &text, grammar));
        }
    }
    sources.files.sort_by(|a, b| a.path.cmp(&b.path));
    sources
}
