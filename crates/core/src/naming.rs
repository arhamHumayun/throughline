//! Plain-language names for flows, from the commits that built them.
//!
//! Agent-written PRs usually have one descriptive commit per step. Each flow
//! takes the subject of the commit whose changed files best match the flow's
//! files (F1 over files, tests and generated files ignored). No LLM.
//!
//! A commit names at most one flow while an unused commit scores above zero for
//! another, so two flows rarely share a title. Flows with no matching commit
//! (or no commits at all, e.g. working-tree diffs) fall back to their root file.

use std::collections::BTreeSet;

use git2::{Repository, Sort};

use crate::flows::{is_generated, is_test_or_story, Flow};

/// One non-merge commit in the analyzed range.
#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub sha: String,
    pub subject: String,
    /// Changed source files (tests, stories, generated files dropped).
    pub files: BTreeSet<String>,
    /// The subset of `files` this commit created.
    pub added: BTreeSet<String>,
}

/// Non-merge commits reachable from `head` and not from `base`, oldest first.
pub fn commits_between(repo: &Repository, base: &str, head: &str) -> Result<Vec<CommitInfo>, git2::Error> {
    let head_oid = repo.revparse_single(head)?.peel_to_commit()?.id();
    let base_oid = repo.revparse_single(base)?.peel_to_commit()?.id();
    let mut walk = repo.revwalk()?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::REVERSE)?;
    walk.push(head_oid)?;
    walk.hide(base_oid)?;

    let mut out = Vec::new();
    for oid in walk {
        let commit = repo.find_commit(oid?)?;
        if commit.parent_count() != 1 {
            continue;
        }
        let parent_tree = commit.parent(0)?.tree()?;
        let diff = repo.diff_tree_to_tree(Some(&parent_tree), Some(&commit.tree()?), None)?;
        let mut files = BTreeSet::new();
        let mut added = BTreeSet::new();
        for d in diff.deltas() {
            let Some(p) = d.new_file().path().or_else(|| d.old_file().path()).and_then(|p| p.to_str()) else {
                continue;
            };
            if is_test_or_story(p) || is_generated(p) {
                continue;
            }
            if d.status() == git2::Delta::Added {
                added.insert(p.to_string());
            }
            files.insert(p.to_string());
        }
        out.push(CommitInfo {
            sha: commit.id().to_string(),
            subject: clean_subject(commit.summary().unwrap_or_default()),
            files,
            added,
        });
    }
    Ok(out)
}

/// Drop a trailing squash-merge PR reference like ` (#29487)`.
fn clean_subject(subject: &str) -> String {
    let s = subject.trim();
    if let Some(i) = s.rfind(" (#") {
        if s.ends_with(')') && s[i + 3..s.len() - 1].chars().all(|c| c.is_ascii_digit()) {
            return s[..i].to_string();
        }
    }
    s.to_string()
}

/// Fallback title: the root file's name without directories or extensions.
pub fn root_title(root: &str) -> String {
    let name = root.rsplit('/').next().unwrap_or(root);
    name.split('.').next().unwrap_or(name).to_string()
}

/// Subjects that describe housekeeping, not a change anyone reviews for.
fn is_housekeeping(subject: &str) -> bool {
    let s = subject.to_lowercase();
    ["merge ", "remove code comments", "fix lint", "lint", "format", "typo", "wip", "regenerate", "bump "]
        .iter()
        .any(|p| s.starts_with(p))
}

/// F1 of the commit's files against the flow's, boosted up to 2x by the share of
/// overlapping files the commit created: the commit that introduces a flow names it
/// better than a later fix-up that touches the same files.
fn score(c: &CommitInfo, flow: &BTreeSet<String>) -> f64 {
    let hit = c.files.intersection(flow).count() as f64;
    if hit == 0.0 {
        return 0.0;
    }
    let p = hit / c.files.len() as f64;
    let r = hit / flow.len() as f64;
    let created = c.added.intersection(flow).count() as f64 / hit;
    (2.0 * p * r / (p + r)) * (1.0 + created)
}

/// Set `title` and `commit` on each flow.
pub fn name_flows(flows: &mut [Flow], commits: &[CommitInfo]) {
    let candidates: Vec<&CommitInfo> =
        commits.iter().filter(|c| !c.files.is_empty() && !is_housekeeping(&c.subject)).collect();
    let flow_files: Vec<BTreeSet<String>> =
        flows.iter().map(|f| f.files.iter().cloned().collect()).collect();

    // All (score, flow, commit) pairs, best first; assign greedily, one commit per flow.
    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for (fi, ff) in flow_files.iter().enumerate() {
        for (ci, c) in candidates.iter().enumerate() {
            let s = score(c, ff);
            if s > 0.0 {
                pairs.push((s, fi, ci));
            }
        }
    }
    pairs.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));

    let mut named = vec![false; flows.len()];
    let mut used = vec![false; candidates.len()];
    for &(_, fi, ci) in &pairs {
        if named[fi] || used[ci] {
            continue;
        }
        named[fi] = true;
        used[ci] = true;
        flows[fi].title = candidates[ci].subject.clone();
        flows[fi].commit = Some(candidates[ci].sha.clone());
    }
    // A flow whose every matching commit went elsewhere may reuse its best one.
    for &(_, fi, ci) in &pairs {
        if !named[fi] {
            named[fi] = true;
            flows[fi].title = candidates[ci].subject.clone();
            flows[fi].commit = Some(candidates[ci].sha.clone());
        }
    }
    for (fi, f) in flows.iter_mut().enumerate() {
        if !named[fi] {
            f.title = root_title(&f.root);
            f.commit = None;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn flow(root: &str, files: &[&str]) -> Flow {
        Flow {
            root: root.into(),
            files: files.iter().map(|s| s.to_string()).collect(),
            tests: vec![],
            title: String::new(),
            commit: None,
        }
    }

    fn commit(sha: &str, subject: &str, files: &[&str]) -> CommitInfo {
        CommitInfo {
            sha: sha.into(),
            subject: subject.into(),
            files: files.iter().map(|s| s.to_string()).collect(),
            added: BTreeSet::new(),
        }
    }

    #[test]
    fn best_matching_commit_names_each_flow() {
        let mut flows = vec![
            flow("api.ts", &["api.ts", "svc.ts"]),
            flow("ui.tsx", &["ui.tsx", "button.tsx"]),
        ];
        let commits = vec![
            commit("1", "Expose the thing over the API", &["api.ts", "svc.ts"]),
            commit("2", "Remove code comments", &["ui.tsx", "button.tsx"]),
            commit("3", "Show the thing in the UI", &["ui.tsx", "button.tsx", "svc.ts"]),
        ];
        name_flows(&mut flows, &commits);
        assert_eq!(flows[0].title, "Expose the thing over the API");
        assert_eq!(flows[1].title, "Show the thing in the UI");
    }

    #[test]
    fn one_commit_per_flow_when_alternatives_exist() {
        let mut flows = vec![flow("a.ts", &["a.ts", "x.ts"]), flow("b.ts", &["b.ts", "x.ts"])];
        let commits = vec![
            commit("1", "Big change", &["a.ts", "b.ts", "x.ts"]),
            commit("2", "Small change to b", &["b.ts"]),
        ];
        name_flows(&mut flows, &commits);
        assert_ne!(flows[0].title, flows[1].title);
    }

    #[test]
    fn the_commit_that_creates_the_files_beats_a_later_fixup() {
        let mut flows = vec![flow("ui.tsx", &["ui.tsx", "list.tsx", "row.tsx"])];
        let mut intro = commit("1", "Render conversations as chat bubbles", &["ui.tsx", "list.tsx", "row.tsx", "api.ts"]);
        intro.added = ["list.tsx", "row.tsx"].iter().map(|s| s.to_string()).collect();
        let fixup = commit("2", "Wait for directions before drawing bubbles", &["ui.tsx", "list.tsx", "row.tsx"]);
        name_flows(&mut flows, &[intro, fixup]);
        assert_eq!(flows[0].title, "Render conversations as chat bubbles");
    }

    #[test]
    fn squash_pr_suffix_is_dropped() {
        assert_eq!(clean_subject("Hide the footer (#29484)"), "Hide the footer");
        assert_eq!(clean_subject("Keep (#notanumber)"), "Keep (#notanumber)");
    }

    #[test]
    fn no_commits_falls_back_to_root_file() {
        let mut flows = vec![flow("src/components/ChatThreadComposer.tsx", &["src/components/ChatThreadComposer.tsx"])];
        name_flows(&mut flows, &[]);
        assert_eq!(flows[0].title, "ChatThreadComposer");
        assert!(flows[0].commit.is_none());
    }
}
