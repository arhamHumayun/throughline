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
use crate::graph::{self, SymbolGraph};
use crate::output::{self, build_analysis_output};
use crate::pipeline;
use crate::rank;
use crate::types::AnalysisOutput;

/// Which changes to analyze.
#[derive(Debug, Clone)]
pub enum DiffSpec {
    /// Committed changes between two refs (merge-base aware), working tree excluded.
    Refs { base: String, head: String },
    /// Branch `base` against the current working tree (your agent's uncommitted output).
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

/// Analyze a diff in the repository containing `repo_path`.
pub fn analyze(repo_path: &Path, spec: &DiffSpec) -> Result<AnalysisOutput, AnalysisError> {
    let repo = Repository::discover(repo_path)?;
    let workdir = repo.workdir().ok_or(AnalysisError::BareRepo)?.to_path_buf();

    let mut config =
        DiffcoreConfig::load_from_dir(&workdir).map_err(|e| AnalysisError::Config(e.to_string()))?;
    config.apply_detected_subtrees(&repo);

    let (diff_result, diff_source) = match spec {
        DiffSpec::Refs { base, head } => {
            let sel = git::diff_refs_with_worktree_fallback(&repo, base, head)?;
            let src = output::diff_source_branch(
                base,
                head,
                sel.diff.base_sha.as_deref(),
                sel.diff.head_sha.as_deref(),
            );
            (sel.diff, src)
        }
        DiffSpec::BranchToWorkdir { base } => {
            let d = git::diff_branch_to_workdir(&repo, base)?;
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
    let mut sym_graph = SymbolGraph::build_with_workspace(&parsed_files, &workspace_map);
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

    Ok(build_analysis_output(
        &diff_result,
        diff_source,
        &parsed_files,
        &cluster_result,
        &ranked,
    ))
}
