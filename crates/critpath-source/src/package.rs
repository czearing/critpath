//! Which dependency a resolved file belongs to, and whether anyone here can change it.
//!
//! Read from the path and nothing else. A package manager states where a dependency's files live
//! -- under a `node_modules` directory, in a folder named for the package, with a scope taking two
//! segments instead of one -- so the question "whose code is this" is already answered by the time
//! a position resolves. No manifest is opened, no registry is consulted, and no name is recognised.

/// Whether the repository under test can change the code at a position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Fixability {
    /// A file the repository owns. A change here is an edit.
    Repository,
    /// A file inside an installed dependency.
    ///
    /// Not editable in place, whatever the measurement says. The available moves are configuring
    /// the dependency, upgrading it, or not calling it, and saying so is the difference between a
    /// finding somebody can act on and one they cannot.
    Dependency,
}

impl Fixability {
    /// How a report names this.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Repository => "in this repository",
            Self::Dependency => "inside a dependency",
        }
    }
}

/// The dependency a path belongs to, if it belongs to one.
///
/// The *last* `node_modules` wins. Installs nest so that different parts of a tree can hold
/// different versions of the same dependency, so a path can contain several; anchoring on the
/// first bills a nested copy to whatever happens to enclose it.
///
/// A leading `@` takes two segments, because a scope is not a package. Taking one would return
/// `@microsoft` for every unrelated library that vendor publishes and merge their costs into a
/// single total that names nothing anyone can act on.
#[must_use]
pub fn package_of(path: &str) -> Option<&str> {
    const MARKER: &str = "node_modules/";
    let start = path.rfind(MARKER)? + MARKER.len();
    let rest = path.get(start..)?;
    let mut segments = rest.split('/');
    let first = segments.next().filter(|segment| !segment.is_empty())?;
    let end = if first.starts_with('@') {
        let second = segments.next().filter(|segment| !segment.is_empty())?;
        first.len() + 1 + second.len()
    } else {
        first.len()
    };
    // A directory called node_modules with nothing under it names no package.
    rest.get(..end).filter(|name| !name.is_empty()).map(|name| &path[start..start + name.len()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scoped_dependency_keeps_both_of_its_segments() {
        assert_eq!(
            package_of("C:/repo/node_modules/@microsoft/oteljs-web/dist/WebSink.js"),
            Some("@microsoft/oteljs-web"),
            "a scope is shared by unrelated packages and names nothing on its own",
        );
    }

    #[test]
    fn an_unscoped_dependency_takes_one_segment() {
        assert_eq!(
            package_of("C:/repo/apps/new-office/node_modules/react-dom/cjs/react-dom.js"),
            Some("react-dom"),
        );
    }

    #[test]
    fn the_innermost_install_wins() {
        // The exact shape a real monorepo produces. Anchoring on the first `node_modules` returns
        // the outer package and bills a nested version's cost to the wrong dependency.
        assert_eq!(
            package_of("/r/node_modules/webpack/node_modules/tapable/lib/Hook.js"),
            Some("tapable"),
        );
    }

    #[test]
    fn a_file_the_repository_owns_belongs_to_no_dependency() {
        assert_eq!(package_of("C:/repo/apps/new-office/src/HomePage.tsx"), None);
        assert_eq!(package_of(""), None);
        // A decoy: a repository directory merely named like the marker is not an install.
        assert_eq!(package_of("C:/repo/src/node_modules_shim/index.ts"), None);
    }

    #[test]
    fn a_marker_with_nothing_under_it_names_nothing() {
        assert_eq!(package_of("C:/repo/node_modules/"), None);
        assert_eq!(package_of("C:/repo/node_modules/@scope/"), None);
    }

    #[test]
    fn fixability_follows_from_whether_a_package_was_named() {
        assert_eq!(Fixability::Repository.word(), "in this repository");
        assert_eq!(Fixability::Dependency.word(), "inside a dependency");
    }
}
