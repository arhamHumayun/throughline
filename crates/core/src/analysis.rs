//! One-call entry point: diff two refs and produce flow groups.
//!
//! Deterministic only. No LLM, no network, no disk cache.

use std::path::Path;

use git2::Repository;

use crate::cluster;
use crate::config::DiffcoreConfig;
use crate::entrypoint;
use crate::flow::{self, FlowConfig};
use crate::git;
use crate::naming;
use crate::flows::{self, Flows};
use crate::graph::{self, Resolver, SymbolGraph};
use crate::tsconfig::PathAliases;
use crate::output::{self, build_analysis_output};
use crate::pipeline;
use crate::rank;
use crate::types::AnalysisOutput;

/// Which changes to analyze.
#[derive(Debug, Clone)]
pub enum DiffSpec {
    /// What `head` added since it forked from `base` (`git diff base...head`), working tree excluded.
    Refs { base: String, head: String },
    /// What the working tree adds since the checkout forked from `base`: commits and
    /// uncommitted edits, untracked files included (your agent's output before you push).
    BranchToWorkdir { base: String },
    Staged,
    Unstaged,
}

#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    #[error("git: {0}")]
    Git(#[from] git2::Error),
    #[error("diff: {0}")]
    Diff(#[from] git::GitError),
    #[error("config: {0}")]
    Config(String),
    #[error("bare repositories are not supported")]
    BareRepo,
}

/// Result of one analysis.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Analysis {
    /// diff-core style groups (kept for comparison while flows mature).
    pub output: AnalysisOutput,
    /// Stack-ordered flows: roots are changed code no other changed code calls.
    pub flows: Flows,
    /// The commit the diff's old side is read from (for `Refs`, the merge base).
    pub base_sha: Option<String>,
    /// The commit the new side is read from; `None` when it is the working tree (or the index).
    pub head_sha: Option<String>,
}

/// Analyze a diff in the repository containing `repo_path`.
/// Offset that sorts import-only uses after every call site.
const IMPORT_ONLY: usize = 1 << 32;

pub fn analyze(repo_path: &Path, spec: &DiffSpec) -> Result<Analysis, AnalysisError> {
    let repo = Repository::discover(repo_path)?;
    let workdir = repo.workdir().ok_or(AnalysisError::BareRepo)?.to_path_buf();

    let mut config =
        DiffcoreConfig::load_from_dir(&workdir).map_err(|e| AnalysisError::Config(e.to_string()))?;
    config.apply_detected_subtrees(&repo);

    let (diff_result, diff_source) = match spec {
        DiffSpec::Refs { base, head } => {
            let d = git::diff_merge_base(&repo, base, head)?;
            let src = output::diff_source_branch(base, head, d.base_sha.as_deref(), d.head_sha.as_deref());
            (d, src)
        }
        DiffSpec::BranchToWorkdir { base } => {
            let d = git::diff_merge_base_to_workdir(&repo, base, "HEAD")?;
            let src = output::diff_source_branch_with_worktree(base, d.base_sha.as_deref());
            (d, src)
        }
        DiffSpec::Staged => (git::diff_staged(&repo)?, output::diff_source_staged()),
        DiffSpec::Unstaged => (git::diff_unstaged(&repo)?, output::diff_source_unstaged()),
    };

    let file_inputs: Vec<(&str, &str)> = diff_result
        .files
        .iter()
        .filter_map(|f| {
            let content = f.new_content.as_deref().or(f.old_content.as_deref())?;
            let path = f.path();
            (!config.is_ignored(path)).then_some((path, content))
        })
        .collect();
    let parsed_files = pipeline::parse_files_parallel(&file_inputs);

    let workspace_map = graph::build_workspace_map(&workdir);
    let paths: Vec<&str> = file_inputs.iter().map(|(p, _)| *p).collect();
    let aliases = PathAliases::load(&workdir, &paths);
    let mut sym_graph =
        SymbolGraph::build_with_resolver(&parsed_files, &Resolver::new(&workspace_map, &aliases));
    let entrypoints = entrypoint::detect_entrypoints(&parsed_files);
    let flow_analysis = flow::analyze_data_flow(&parsed_files, &FlowConfig::default());
    flow::enrich_graph(&mut sym_graph, &flow_analysis);

    let changed_files: Vec<String> = diff_result
        .files
        .iter()
        .filter(|f| !config.is_ignored(f.path()))
        .map(|f| f.path().to_string())
        .collect();
    let cluster_result = cluster::cluster_files(&sym_graph, &entrypoints, &changed_files);

    let file_centrality = sym_graph.file_centrality();
    let rank_inputs = rank::build_rank_inputs(&cluster_result, &file_centrality);
    let ranked = rank::rank_groups(&rank_inputs, &config.ranking);

    // Order each tree's siblings by where the parent first uses them: the first call
    // (JSX counts) of any symbol the edge reaches. Siblings never called (types, constants)
    // follow all called ones, in import order.
    let by_path: std::collections::HashMap<&str, &crate::ast::ParsedFile> =
        parsed_files.iter().map(|f| (f.path.as_str(), f)).collect();
    let first_use = |parent: &str, names: &[String]| -> Option<usize> {
        let f = by_path.get(parent)?;
        // Local names: the symbols themselves, plus any alias the parent imports them under.
        let mut local: Vec<&str> = names.iter().map(String::as_str).collect();
        for i in &f.imports {
            for n in &i.names {
                if let Some(a) = n.alias.as_deref().filter(|_| names.contains(&n.name)) {
                    local.push(a);
                }
            }
        }
        let hit = |callee: &str| {
            let last = callee.rsplit(['.', ':']).next().unwrap_or(callee);
            local.iter().any(|n| *n == callee || *n == last)
        };
        f.call_sites.iter().filter(|c| hit(&c.callee)).map(|c| c.line).min().or_else(|| {
            f.imports
                .iter()
                .filter(|i| i.names.iter().any(|n| names.iter().any(|m| *m == n.name || Some(m) == n.alias.as_ref())))
                .map(|i| IMPORT_ONLY + i.line)
                .min()
        })
    };
    let mut flows = flows::compute_ordered(&sym_graph, &changed_files, &first_use);
    let range = match spec {
        DiffSpec::Refs { base, head } => Some((base.as_str(), head.as_str())),
        DiffSpec::BranchToWorkdir { base } => Some((base.as_str(), "HEAD")),
        _ => None,
    };
    if let Some((base, head)) = range {
        // Naming is best effort: a failure here shouldn't fail the analysis.
        if let Ok(commits) = naming::commits_between(&repo, base, head) {
            naming::name_flows(&mut flows.flows, &commits);
        }
    }
    let output = build_analysis_output(&diff_result, diff_source, &parsed_files, &cluster_result, &ranked);
    let (base_sha, head_sha) = (diff_result.base_sha.clone(), diff_result.head_sha.clone());
    Ok(Analysis { output, flows, base_sha, head_sha })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use git2::{Signature, Time};

    fn commit(repo: &Repository, msg: &str) -> git2::Oid {
        let mut idx = repo.index().unwrap();
        idx.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None).unwrap();
        idx.write().unwrap();
        let tree = repo.find_tree(idx.write_tree().unwrap()).unwrap();
        let sig = Signature::new("t", "t@e", &Time::new(0, 0)).unwrap();
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parents).unwrap()
    }

    fn changed(a: &Analysis) -> Vec<String> {
        let mut v: Vec<String> = a.output.groups.iter().flat_map(|g| g.files.iter().map(|f| f.path.clone())).collect();
        v.extend(a.output.infrastructure_group.iter().flat_map(|g| g.files.iter().cloned()));
        v.sort();
        v
    }

    /// Comparing a branch shows what it added since it forked, not what its base did since,
    /// and the working tree option adds uncommitted and untracked files on top.
    #[test]
    fn compares_from_the_merge_base() {
        let dir = tempfile::TempDir::new().unwrap();
        let w = |p: &str, s: &str| std::fs::write(dir.path().join(p), s).unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        w("a.ts", "export function a() { return 1; }\n");
        let root = commit(&repo, "init");
        let root = repo.find_commit(root).unwrap();
        repo.branch("base", &root, true).unwrap();
        repo.branch("feature", &root, true).unwrap();
        // The base moves on after the fork.
        repo.set_head("refs/heads/base").unwrap();
        w("later.ts", "export function later() { return 2; }\n");
        commit(&repo, "base moves on");
        // The feature branch adds its own file.
        repo.set_head("refs/heads/feature").unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force().remove_untracked(true))).unwrap();
        w("feat.ts", "import { a } from './a';\nexport function feat() { return a(); }\n");
        let feat = commit(&repo, "add feat");

        let a = analyze(dir.path(), &DiffSpec::Refs { base: "base".into(), head: "feature".into() }).unwrap();
        assert_eq!(changed(&a), vec!["feat.ts"]);
        assert_eq!(a.base_sha, Some(root.id().to_string()));
        assert_eq!(a.head_sha, Some(feat.to_string()));

        w("wip.ts", "export function wip() { return 3; }\n");
        let a = analyze(dir.path(), &DiffSpec::BranchToWorkdir { base: "base".into() }).unwrap();
        assert_eq!(changed(&a), vec!["feat.ts", "wip.ts"]);
        assert_eq!(a.base_sha, Some(root.id().to_string()));
        assert_eq!(a.head_sha, None);
    }
}
