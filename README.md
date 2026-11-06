# visual-diff (working name)

Cargo workspace:

- `crates/core` (`diffview-core`): pure data, no UI. git2 diff, tree-sitter IR (TypeScript/JavaScript + Python), symbol graph, entrypoint detection, flow groups, ranking. One call: `diffview_core::analysis::analyze(repo, &DiffSpec::Refs { base, head })`.
- `crates/app` (`diffview-app`, bin `diffview`): overview window on [Rune](https://github.com/anomalyco/rune) (pinned rev 5521783), wired to core. Repo and refs come from env DIFFVIEW_REPO / DIFFVIEW_BASE / DIFFVIEW_HEAD. `diffview --snap out.png` renders headless. Linux builds need libxkbcommon-dev and libwayland-dev.

## Provenance

Strip-mined from [diff-core](https://github.com/jamesaphoenix/diff-core) (MIT) at commit `7b82ed8`. Removed: LLM providers and refinement, eval harness, embeddings, PR-URL fetching, analysis cache, groups manifest, Tauri/CLI/VS Code, and every language except TS/JS and Python. Lifted the CLI's analyze pipeline into `analysis.rs`. Output is identical to diff-core's deterministic mode on the milestone-0 fixture PRs (twentyhq/twenty #28927, #29326, #29327).

## Known gaps (from milestone 0), next work in `core`

1. Roots: tests are treated as entry points. Use changed symbols with no changed caller; put tests and stories in a side bucket.
2. Resolve tsconfig `paths`/`baseUrl` aliases (`@/…`, `src/…`). Without it, alias-heavy repos get no edges.
3. Treat JSX `<Component/>` usage as a call edge.
4. `config.rs` still carries LLM config structs; trim.

## Dev

    cargo test --release
    cargo run --release --example analyze --  (, bin ): overview window on [Rune](https://github.com/anomalyco/rune) (pinned rev 5521783), wired to core. Repo/refs via env DIFFVIEW_REPO / DIFFVIEW_BASE / DIFFVIEW_HEAD.  renders headless. Linux needs libxkbcommon-dev, libwayland-dev (windowed runs also libxkbcommon-x11-0).
