//! Local repositories: what the picker needs to open one and choose what to compare, and the
//! list of recently opened repos.

use std::path::{Path, PathBuf};

use git2::{BranchType, Repository, StatusOptions};

/// A local repository, as the picker shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalRepo {
    /// The working tree's root (the folder the user picked may be inside it).
    pub root: PathBuf,
    /// The root folder's name.
    pub name: String,
    /// Local branches: the checked-out one first, then the default branch, then by most
    /// recent commit.
    pub branches: Vec<String>,
    /// The checked-out branch; `None` when HEAD is detached or unborn.
    pub current: Option<String>,
    /// main, master or develop if one exists, else the first branch.
    pub default_branch: Option<String>,
    /// Whether the working tree has uncommitted changes (untracked files count).
    pub dirty: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("{0} is not inside a git repository")]
    NotARepo(String),
    #[error("{0} is a bare repository; open a checkout")]
    Bare(String),
    #[error("git: {0}")]
    Git(#[from] git2::Error),
}

/// Open the repository containing `path`.
pub fn open(path: &Path) -> Result<LocalRepo, OpenError> {
    let repo = Repository::discover(path).map_err(|_| OpenError::NotARepo(path.display().to_string()))?;
    let root = repo.workdir().ok_or_else(|| OpenError::Bare(path.display().to_string()))?;
    // git2 gives the workdir with a trailing separator; drop it so names and recents match.
    let root = root.components().collect::<PathBuf>();
    let name = root.file_name().map_or_else(|| root.display().to_string(), |n| n.to_string_lossy().into_owned());
    let current = repo.head().ok().filter(|h| h.is_branch()).and_then(|h| h.shorthand().map(String::from));
    let mut branches: Vec<(String, i64)> = Vec::new();
    for b in repo.branches(Some(BranchType::Local))? {
        let (b, _) = b?;
        let Some(n) = b.name()?.map(String::from) else { continue };
        let when = b.get().peel_to_commit().map(|c| c.time().seconds()).unwrap_or(0);
        branches.push((n, when));
    }
    let default_branch = ["main", "master", "develop"]
        .iter()
        .find(|d| branches.iter().any(|(n, _)| n == *d))
        .map(|d| d.to_string())
        .or_else(|| branches.first().map(|(n, _)| n.clone()));
    let rank = |n: &str| {
        if Some(n) == current.as_deref() {
            0
        } else if Some(n) == default_branch.as_deref() {
            1
        } else {
            2
        }
    };
    branches.sort_by(|a, b| rank(&a.0).cmp(&rank(&b.0)).then(b.1.cmp(&a.1)).then(a.0.cmp(&b.0)));
    let mut opts = StatusOptions::new();
    opts.include_untracked(true).include_ignored(false).exclude_submodules(true);
    let dirty = repo.statuses(Some(&mut opts)).map(|s| !s.is_empty()).unwrap_or(false);
    Ok(LocalRepo { root, name, branches: branches.into_iter().map(|(n, _)| n).collect(), current, default_branch, dirty })
}

/// A file's text in the working tree, `None` if it isn't there (or isn't UTF-8).
pub fn workdir_text(root: &Path, rel: &str) -> Option<String> {
    std::fs::read_to_string(root.join(rel)).ok()
}

/// Most recently opened repos first, at most [`Recent::MAX`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Recent {
    pub paths: Vec<PathBuf>,
}

impl Recent {
    pub const MAX: usize = 8;

    /// Read the list from `file`, one path per line. A missing or unreadable file is an empty list.
    pub fn load(file: &Path) -> Recent {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        let paths = text.lines().map(str::trim).filter(|l| !l.is_empty()).map(PathBuf::from).take(Self::MAX).collect();
        Recent { paths }
    }

    pub fn save(&self, file: &Path) -> std::io::Result<()> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut out = String::new();
        for p in &self.paths {
            out.push_str(&p.to_string_lossy());
            out.push('\n');
        }
        std::fs::write(file, out)
    }

    /// Move `path` to the front.
    pub fn push(&mut self, path: &Path) {
        self.paths.retain(|p| p != path);
        self.paths.insert(0, path.to_path_buf());
        self.paths.truncate(Self::MAX);
    }

    /// Recent repos that still exist.
    pub fn existing(&self) -> impl Iterator<Item = &PathBuf> {
        self.paths.iter().filter(|p| p.is_dir())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use git2::Signature;

    fn commit(repo: &Repository, msg: &str, time: i64) {
        let mut idx = repo.index().unwrap();
        idx.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None).unwrap();
        idx.write().unwrap();
        let tree = repo.find_tree(idx.write_tree().unwrap()).unwrap();
        let sig = Signature::new("t", "t@e", &git2::Time::new(time, 0)).unwrap();
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parents).unwrap();
    }

    #[test]
    fn open_orders_branches_and_sees_dirt() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        std::fs::write(dir.path().join("a.ts"), "export const a = 1;\n").unwrap();
        commit(&repo, "init", 100);
        // Name the first branch main whatever init.defaultBranch says.
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("main", &head, true).unwrap();
        repo.set_head("refs/heads/main").unwrap();
        for b in repo.branches(Some(BranchType::Local)).unwrap() {
            let (mut b, _) = b.unwrap();
            if b.name().unwrap() != Some("main") {
                b.delete().unwrap();
            }
        }
        repo.branch("old", &head, false).unwrap();
        std::fs::write(dir.path().join("a.ts"), "export const a = 2;\n").unwrap();
        commit(&repo, "two", 200);
        let two = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature", &two, false).unwrap();
        repo.set_head("refs/heads/feature").unwrap();

        let r = open(&dir.path().join(".")).unwrap();
        assert_eq!(r.branches, vec!["feature", "main", "old"]);
        assert_eq!(r.current.as_deref(), Some("feature"));
        assert_eq!(r.default_branch.as_deref(), Some("main"));
        assert!(!r.dirty);
        assert_eq!(r.name, dir.path().file_name().unwrap().to_string_lossy());

        std::fs::write(dir.path().join("b.ts"), "x").unwrap();
        assert!(open(dir.path()).unwrap().dirty);
        assert_eq!(workdir_text(&r.root, "b.ts").as_deref(), Some("x"));
    }

    #[test]
    fn open_rejects_plain_folders() {
        let dir = tempfile::TempDir::new().unwrap();
        // A temp dir could sit inside a repo; only check the error kind when it doesn't.
        if Repository::discover(dir.path()).is_err() {
            assert!(matches!(open(dir.path()), Err(OpenError::NotARepo(_))));
        }
    }

    #[test]
    fn recent_round_trips_and_caps() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("cfg").join("recent");
        let mut r = Recent::load(&file);
        assert!(r.paths.is_empty());
        for i in 0..10 {
            r.push(Path::new(&format!("/r{i}")));
        }
        r.push(Path::new("/r5"));
        assert_eq!(r.paths.len(), Recent::MAX);
        assert_eq!(r.paths[0], PathBuf::from("/r5"));
        assert_eq!(r.paths.iter().filter(|p| **p == PathBuf::from("/r5")).count(), 1);
        r.save(&file).unwrap();
        assert_eq!(Recent::load(&file), r);
    }
}
