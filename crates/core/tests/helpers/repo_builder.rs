//! Test repo builder and pipeline runner (lifted from diff-core eval/fixtures.rs).
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]
use std::path::Path;

use git2::{Repository, Signature, Time};
use tempfile::TempDir;

use diffview_core::cluster;
use diffview_core::entrypoint;
use diffview_core::flow::{self, FlowConfig};
use diffview_core::graph::SymbolGraph;
use diffview_core::output::{self, build_analysis_output};
use diffview_core::rank::{self};
use diffview_core::types::{AnalysisOutput, RankWeights};
pub struct RepoBuilder {
    dir: TempDir,
    repo: Repository,
}

impl RepoBuilder {
    pub fn new() -> Self {
        let dir = TempDir::new().expect("failed to create temp dir");
        let repo = Repository::init(dir.path()).expect("failed to init repo");

        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test User").unwrap();
        config.set_str("user.email", "test@example.com").unwrap();

        Self { dir, repo }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Write a file relative to the repo root.
    pub fn write_file(&self, rel_path: &str, content: &str) {
        let full = self.dir.path().join(rel_path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full, content).unwrap();
    }

    /// Stage all changes and commit with a message. Returns the commit OID.
    ///
    /// Uses a fixed Unix-epoch timestamp so commit SHAs are reproducible
    /// across test runs. This is required for VCR cassette caching in
    /// integration tests like `adversarial_refinement` — without it, every
    /// run produces a fresh `analysis_json` (which embeds the SHAs) and the
    /// cache key never hits.
    pub fn commit(&self, message: &str) -> git2::Oid {
        let mut index = self.repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();

        let tree_oid = index.write_tree().unwrap();
        let tree = self.repo.find_tree(tree_oid).unwrap();
        // Fixed timestamp: 2024-01-01 00:00:00 UTC. Using `Signature::now`
        // here would inject wall-clock time into every commit hash.
        let time = Time::new(1_704_067_200, 0);
        let sig = Signature::new("Test User", "test@example.com", &time).unwrap();

        let parent = self.repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();

        self.repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .unwrap()
    }

    /// Create a branch at the current HEAD. No-op if it already exists.
    pub fn create_branch(&self, name: &str) {
        let head = self.repo.head().unwrap().peel_to_commit().unwrap();
        let _ = self.repo.branch(name, &head, false);
    }

    /// Checkout a branch by name.
    pub fn checkout(&self, name: &str) {
        let ref_name = format!("refs/heads/{}", name);
        let obj = self.repo.revparse_single(&ref_name).unwrap();
        self.repo.checkout_tree(&obj, None).unwrap();
        self.repo.set_head(&ref_name).unwrap();
    }

    /// Get a reference to the underlying git2 Repository.
    pub fn repo(&self) -> &Repository {
        &self.repo
    }

    /// Get a reference to the underlying TempDir (for keeping it alive).
    pub fn temp_dir(&self) -> &TempDir {
        &self.dir
    }
}

impl Default for RepoBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Pipeline Runner
// ═══════════════════════════════════════════════════════════════════════════

/// Run the full pipeline on a repo diff between two refs.
pub fn run_pipeline(repo_path: &Path, base_ref: &str, head_ref: &str) -> AnalysisOutput {
    let repo = Repository::open(repo_path).expect("failed to open repo");
    let diff_result = diffview_core::git::diff_refs(&repo, base_ref, head_ref).expect("diff_refs failed");

    // Use shared QueryEngine for parsing — supports all languages including Rust
    use std::sync::OnceLock;
    static ENGINE: OnceLock<diffview_core::query_engine::QueryEngine> = OnceLock::new();
    let engine = ENGINE.get_or_init(|| {
        diffview_core::query_engine::QueryEngine::new().expect("failed to create query engine")
    });
    let mut parsed_files = Vec::new();
    for file_diff in &diff_result.files {
        if let Some(ref content) = file_diff.new_content {
            let path = file_diff.path();
            if let Ok(parsed) = engine.parse_file(path, content) {
                parsed_files.push(parsed);
            }
        }
    }

    let mut graph = SymbolGraph::build(&parsed_files);
    let entrypoints = entrypoint::detect_entrypoints(&parsed_files);
    let flow_analysis = flow::analyze_data_flow(&parsed_files, &FlowConfig::default());
    flow::enrich_graph(&mut graph, &flow_analysis);

    let changed_files: Vec<String> = diff_result
        .files
        .iter()
        .map(|f| f.path().to_string())
        .collect();
    let cluster_result = cluster::cluster_files(&graph, &entrypoints, &changed_files);

    let weights = RankWeights::default();
    let file_centrality = graph.file_centrality();
    let rank_inputs = rank::build_rank_inputs(&cluster_result, &file_centrality);
    let ranked = rank::rank_groups(&rank_inputs, &weights);

    let diff_source = output::diff_source_branch(
        base_ref,
        head_ref,
        diff_result.base_sha.as_deref(),
        diff_result.head_sha.as_deref(),
    );

    build_analysis_output(
        &diff_result,
        diff_source,
        &parsed_files,
        &cluster_result,
        &ranked,
    )
}

/// Find the feature branch name in a repo (first non-main branch).
pub fn find_feature_branch(repo_path: &Path) -> String {
    let repo = Repository::open(repo_path).unwrap();
    let branches = repo.branches(Some(git2::BranchType::Local)).unwrap();
    for branch in branches {
        let (branch, _) = branch.unwrap();
        let name = branch.name().unwrap().unwrap().to_string();
        if name != "main" {
            return name;
        }
    }
    panic!("No feature branch found");
}

// ═══════════════════════════════════════════════════════════════════════════
// Fixture 1: TypeScript Express API with services + DB layer
// ═══════════════════════════════════════════════════════════════════════════

