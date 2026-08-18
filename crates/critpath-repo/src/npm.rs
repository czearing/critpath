//! Reading a repository whose components are directories and whose manifests are JSON.
//!
//! This is the only module that knows what a manifest looks like, exactly as the trace side keeps
//! Trace Event Format in one reader. Everything downstream sees components and edges.
//!
//! Nothing here builds, installs, resolves a registry or executes a line of the subject. It reads
//! files that are already on disk and measures them.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::{Component, ComponentId, Repo};

/// Why a repository could not be read at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The directory holds no manifest, so nothing states what depends on what.
    NoManifest(String),
    /// The manifest is not readable as JSON.
    Unreadable(String),
    /// No entry was named and the repository holds more than one, so any choice would be a guess.
    ManyEntries(Vec<String>),
    /// The named entry is not a component of this repository.
    NoSuchEntry(String),
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoManifest(at) => {
                write!(f, "no manifest at {at}, so nothing states what depends on what")
            }
            Self::Unreadable(at) => write!(f, "the manifest at {at} is not readable as JSON"),
            Self::ManyEntries(names) => write!(
                f,
                "this repository holds {} things that can be shipped, and which one you meant is \
                 not a fact about the repository. Name one with --entry: {}",
                names.len(),
                names.join(", ")
            ),
            Self::NoSuchEntry(name) => write!(f, "{name} is not a component of this repository"),
        }
    }
}

/// Reads a repository rooted at `root`, measuring what is on disk and resolving what it declares.
///
/// # Errors
/// Refuses when there is no manifest, when it cannot be parsed, or when the entry is ambiguous.
pub fn read(root: &Path, entry: Option<&str>) -> Result<Repo, Refusal> {
    let mut repo = Repo::default();
    let mut by_directory: HashMap<PathBuf, ComponentId> = HashMap::new();

    let root_manifest = manifest_at(root)?;
    let members = workspace_members(root, &root_manifest);

    // Owned components first, so an installed copy of a workspace package never shadows the source.
    let mut owned = Vec::new();
    for directory in members {
        let Ok(manifest) = manifest_at(&directory) else {
            repo.refusals.push(format!("{} has no readable manifest", show(&directory)));
            continue;
        };
        let Some(name) = manifest.get("name").and_then(|n| n.as_str()).map(ToOwned::to_owned)
        else {
            repo.refusals.push(format!("{} declares no name", show(&directory)));
            continue;
        };
        let id = push(&mut repo, &mut by_directory, &name, &directory, true);
        owned.push((id, manifest));
        repo.entries.push(name);
    }
    repo.entries.sort();

    let owned_by_name: HashMap<String, ComponentId> =
        owned.iter().map(|(id, _)| (repo.components[*id].name.clone(), *id)).collect();

    // Then everything installed, discovered by following declarations rather than by guessing.
    let mut pending: Vec<(ComponentId, serde_json::Value)> = owned;
    while let Some((id, manifest)) = pending.pop() {
        let directory = PathBuf::from(&repo.components[id].directory);
        for name in declared(&manifest) {
            if let Some(&member) = owned_by_name.get(&name) {
                if !repo.components[id].declares.contains(&member) {
                    repo.components[id].declares.push(member);
                }
                continue;
            }
            let Some(found) = resolve(&directory, root, &name) else {
                repo.components[id].unresolved.push(name);
                continue;
            };
            if let Some(&known) = by_directory.get(&found) {
                if !repo.components[id].declares.contains(&known) {
                    repo.components[id].declares.push(known);
                }
                continue;
            }
            let Ok(found_manifest) = manifest_at(&found) else {
                repo.components[id].unresolved.push(name);
                continue;
            };
            let child = push(&mut repo, &mut by_directory, &name, &found, false);
            repo.components[id].declares.push(child);
            pending.push((child, found_manifest));
        }
    }

    // Weight every component that was reached by declaration, and every installed directory that
    // was not, because "installed and never declared" is a finding and cannot be found by
    // following declarations.
    for extra in installed_under(root) {
        if by_directory.contains_key(&extra) {
            continue;
        }
        let Ok(manifest) = manifest_at(&extra) else {
            continue;
        };
        let name = manifest.get("name").and_then(|n| n.as_str()).unwrap_or("").to_owned();
        if name.is_empty() {
            continue;
        }
        push(&mut repo, &mut by_directory, &name, &extra, false);
    }

    weigh_all(&mut repo);
    repo.installed = repo.components.iter().filter(|c| !c.owned).map(|c| c.weight).sum();

    repo.entry = match entry {
        Some(name) => {
            *owned_by_name.get(name).ok_or_else(|| Refusal::NoSuchEntry(name.to_owned()))?
        }
        None => match repo.entries.len() {
            0 => return Err(Refusal::NoManifest(show(root))),
            1 => owned_by_name[&repo.entries[0]],
            _ => return Err(Refusal::ManyEntries(repo.entries.clone())),
        },
    };
    Ok(repo)
}

fn push(
    repo: &mut Repo,
    by_directory: &mut HashMap<PathBuf, ComponentId>,
    name: &str,
    directory: &Path,
    owned: bool,
) -> ComponentId {
    let id = repo.components.len();
    repo.components.push(Component {
        name: name.to_owned(),
        directory: show(directory),
        weight: 0,
        declares: Vec::new(),
        unresolved: Vec::new(),
        owned,
    });
    by_directory.insert(directory.to_path_buf(), id);
    repo.by_name.entry(name.to_owned()).or_insert(id);
    id
}

fn manifest_at(directory: &Path) -> Result<serde_json::Value, Refusal> {
    let path = directory.join("package.json");
    let text = fs::read_to_string(&path).map_err(|_| Refusal::NoManifest(show(directory)))?;
    serde_json::from_str(&text).map_err(|_| Refusal::Unreadable(show(&path)))
}

/// The names a manifest declares as needed to ship.
///
/// Development and peer dependencies are excluded on purpose: they are not part of what a user
/// receives, and counting them would inflate every number in the report.
fn declared(manifest: &serde_json::Value) -> Vec<String> {
    let mut names = Vec::new();
    for field in ["dependencies", "optionalDependencies"] {
        if let Some(map) = manifest.get(field).and_then(|d| d.as_object()) {
            names.extend(map.keys().cloned());
        }
    }
    names
}

/// Workspace member directories, expanded from the patterns the root manifest states.
fn workspace_members(root: &Path, manifest: &serde_json::Value) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    let patterns = match manifest.get("workspaces") {
        Some(serde_json::Value::Array(list)) => list.clone(),
        Some(object) => {
            object.get("packages").and_then(|p| p.as_array()).cloned().unwrap_or_default()
        }
        None => Vec::new(),
    };
    for pattern in patterns.iter().filter_map(|p| p.as_str()) {
        let Some(prefix) = pattern.strip_suffix("/*") else {
            out.push(root.join(pattern));
            continue;
        };
        let Ok(entries) = fs::read_dir(root.join(prefix)) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.path().join("package.json").is_file() {
                out.push(entry.path());
            }
        }
    }
    out
}

/// Finds the directory a declared name resolves to, by the ordinary rule: nearest first, then up.
fn resolve(from: &Path, root: &Path, name: &str) -> Option<PathBuf> {
    let mut at = Some(from);
    while let Some(directory) = at {
        let candidate = directory.join("node_modules").join(name);
        if candidate.join("package.json").is_file() {
            return Some(candidate);
        }
        if directory == root {
            break;
        }
        at = directory.parent();
    }
    None
}

/// Every installed package directory anywhere under the root, including nested installs.
fn installed_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut queue = vec![root.to_path_buf()];
    let mut guard = 0usize;
    while let Some(directory) = queue.pop() {
        guard += 1;
        // A repository with a pathological symlink loop should degrade to a partial census rather
        // than never returning. The bound is far above any real install.
        if guard > 200_000 {
            break;
        }
        let modules = directory.join("node_modules");
        let Ok(entries) = fs::read_dir(&modules) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let scoped =
                path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('@'));
            if scoped {
                if let Ok(inner) = fs::read_dir(&path) {
                    for one in inner.flatten() {
                        if one.path().is_dir() {
                            found.push(one.path());
                            queue.push(one.path());
                        }
                    }
                }
                continue;
            }
            found.push(path.clone());
            queue.push(path);
        }
    }
    found
}

/// Weighs every component, several at a time.
///
/// Weighing is entirely waiting on the file system, so one thread per component-in-flight spends
/// the wait usefully. The work is read-only and each component writes only its own slot, so the
/// result does not depend on how the work was divided.
fn weigh_all(repo: &mut Repo) {
    let directories: Vec<PathBuf> =
        repo.components.iter().map(|c| PathBuf::from(&c.directory)).collect();
    let next = AtomicUsize::new(0);
    let weights: Vec<Mutex<u64>> = (0..directories.len()).map(|_| Mutex::new(0)).collect();
    let hands = std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get).min(16);
    std::thread::scope(|scope| {
        for _ in 0..hands {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(directory) = directories.get(index) else {
                    return;
                };
                *weights[index].lock().expect("weight slot") = weigh(directory);
            });
        }
    });
    for (component, weight) in repo.components.iter_mut().zip(weights) {
        component.weight = weight.into_inner().expect("weight slot");
    }
}

/// Bytes a component occupies, excluding nested installs, because those are their own components.
fn weigh(directory: &Path) -> u64 {
    let mut total = 0;
    let mut queue = vec![directory.to_path_buf()];
    while let Some(at) = queue.pop() {
        let Ok(entries) = fs::read_dir(&at) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                if entry.file_name() == "node_modules" {
                    continue;
                }
                queue.push(entry.path());
            } else if kind.is_file() {
                if let Ok(data) = entry.metadata() {
                    total += data.len();
                }
            }
        }
    }
    total
}

fn show(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::{declared, resolve, weigh, workspace_members};
    use std::fs;

    fn scratch(name: &str) -> std::path::PathBuf {
        let at = std::env::temp_dir().join(format!("critpath-repo-{name}"));
        let _ = fs::remove_dir_all(&at);
        fs::create_dir_all(&at).expect("scratch");
        at
    }

    fn write(at: &std::path::Path, body: &str) {
        fs::create_dir_all(at.parent().expect("parent")).expect("dirs");
        fs::write(at, body).expect("write");
    }

    #[test]
    fn development_dependencies_are_not_counted_as_shipped() {
        let manifest = serde_json::json!({
            "dependencies": { "shipped": "1" },
            "devDependencies": { "not-shipped": "1" },
            "peerDependencies": { "also-not": "1" },
            "optionalDependencies": { "maybe": "1" }
        });
        let mut names = declared(&manifest);
        names.sort();
        assert_eq!(names, vec!["maybe".to_owned(), "shipped".to_owned()]);
    }

    #[test]
    fn resolution_prefers_the_nearest_install() {
        let root = scratch("nearest");
        write(&root.join("node_modules/dep/package.json"), "{}");
        write(&root.join("app/node_modules/dep/package.json"), "{}");
        let found = resolve(&root.join("app"), &root, "dep").expect("resolved");
        assert!(found.starts_with(root.join("app")), "the nearer copy wins");
    }

    #[test]
    fn a_nested_install_is_not_weighed_into_its_host() {
        let root = scratch("weigh");
        write(&root.join("pkg/index.js"), "0123456789");
        write(&root.join("pkg/node_modules/inner/big.js"), &"x".repeat(5000));
        assert_eq!(weigh(&root.join("pkg")), 10);
    }

    #[test]
    fn workspace_patterns_only_admit_directories_holding_a_manifest() {
        let root = scratch("members");
        write(&root.join("package.json"), r#"{"workspaces":["packages/*"]}"#);
        write(&root.join("packages/real/package.json"), r#"{"name":"real"}"#);
        fs::create_dir_all(root.join("packages/empty")).expect("dirs");
        let manifest = serde_json::json!({ "workspaces": ["packages/*"] });
        let members = workspace_members(&root, &manifest);
        assert_eq!(members.len(), 2, "the root and one real member");
        assert!(members.iter().any(|m| m.ends_with("real")));
    }
}
