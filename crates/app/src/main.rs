//! throughline: overview of a diff's flows, and a split diff of one flow.
//! Repo and refs come from env: THROUGHLINE_REPO (default "."), THROUGHLINE_BASE ("main"), THROUGHLINE_HEAD ("HEAD").
//! `throughline` opens the window; `throughline --snap out.png` renders it headless.
//! THROUGHLINE_OPEN=<n> starts on flow n's diff (0-based), for headless snaps.
//!
//! Overview: click a flow to open its diff. Diff: Escape goes back; j/k move between
//! changes, ]/[ between files.

use rune::tok::type_;
use rune::{AppSpec, Cx, El, Key, KeyEvent, Tx, col, row, text};
use rune_kit::Kit;
use rune_kit::diff::{DiffMode, DiffState, DiffView, FileDiff};
use throughline_core::analysis::{DiffSpec, analyze};
use throughline_core::flows::Flow;

struct Repo {
    path: std::path::PathBuf,
    base: String,
    head: String,
}

struct State {
    repo: Repo,
    title: String,
    flows: Vec<Flow>,
    standalone: Vec<String>,
    error: Option<String>,
    /// The open flow and its diff; `None` shows the overview.
    open: Option<(usize, DiffState)>,
}

fn stem(p: &str) -> &str {
    let name = p.rsplit('/').next().unwrap_or(p);
    name.split('.').next().unwrap_or(name)
}

fn init() -> State {
    let var = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    let repo = Repo {
        path: var("THROUGHLINE_REPO", ".").into(),
        base: var("THROUGHLINE_BASE", "main"),
        head: var("THROUGHLINE_HEAD", "HEAD"),
    };
    let spec = DiffSpec::Refs { base: repo.base.clone(), head: repo.head.clone() };
    let mut s = match analyze(&repo.path, &spec) {
        Ok(a) => State {
            title: format!(
                "{} files, {} flows ({}...{})",
                a.output.summary.total_files_changed,
                a.flows.flows.len(),
                repo.base,
                repo.head
            ),
            flows: a.flows.flows,
            standalone: a.flows.standalone,
            error: None,
            open: None,
            repo,
        },
        Err(e) => State {
            title: "throughline".into(),
            flows: vec![],
            standalone: vec![],
            error: Some(format!("core: {e}")),
            open: None,
            repo,
        },
    };
    if let Some(i) = std::env::var("THROUGHLINE_OPEN").ok().and_then(|v| v.parse().ok()) {
        open_flow(&mut s, i);
    }
    s
}

/// Files in reading order: the root first, then the rest of the flow, then its tests.
fn flow_paths(f: &Flow) -> Vec<&str> {
    let mut out = vec![f.root.as_str()];
    out.extend(f.files.iter().map(String::as_str).filter(|p| *p != f.root));
    out.extend(f.tests.iter().map(String::as_str));
    out
}

/// Base and head text of each file, diffed. Added files have an empty base, deleted an empty head.
fn load_diffs(repo: &Repo, paths: &[&str]) -> Result<Vec<FileDiff>, String> {
    let r = git2::Repository::open(&repo.path).map_err(|e| e.to_string())?;
    let at = |rev: &str, p: &str| throughline_core::git::file_content_at_ref(&r, rev, p).map_err(|e| e.to_string());
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        let old = at(&repo.base, p)?.unwrap_or_default();
        let new = at(&repo.head, p)?.unwrap_or_default();
        out.push(FileDiff::from_texts(*p, &old, &new));
    }
    Ok(out)
}

fn open_flow(s: &mut State, i: usize) {
    let Some(f) = s.flows.get(i) else { return };
    match load_diffs(&s.repo, &flow_paths(f)) {
        Ok(files) => {
            let mut d = DiffState::new(files);
            d.set_mode(DiffMode::Split);
            s.open = Some((i, d));
        }
        Err(e) => s.error = Some(format!("loading {}: {e}", f.title)),
    }
}

fn overview(s: &State) -> El {
    let k = Kit::neutral(true);
    let mut list = col().gap(4.0);
    for (i, f) in s.flows.iter().enumerate() {
        let n = flow_paths(f).len();
        let item = row()
            .key(format!("flow-{i}"))
            .padding(8.0)
            .gap(12.0)
            .radius(6.0)
            .items_center()
            .child(text(f.title.as_str()).color(k.c.text_base))
            .child(text(format!("{}, {n} files", stem(&f.root))).color(k.c.text_muted))
            .on_press(move |s: &mut State| open_flow(s, i));
        list = list.child(item);
    }
    if !s.standalone.is_empty() {
        let names: Vec<&str> = s.standalone.iter().map(|p| stem(p)).collect();
        list = list.child(text(format!("Standalone: {}", names.join(", "))).color(k.c.text_muted));
    }
    let mut c = col()
        .w_full()
        .h_full()
        .padding(16.0)
        .gap(12.0)
        .fill(k.c.bg_base)
        .child(text(s.title.as_str()).font(type_::TITLE).color(k.c.text_base));
    if let Some(e) = &s.error {
        c = c.child(text(e.as_str()).color(k.c.danger.fg));
    }
    c.child(list)
}

fn view(s: &State, _: &mut Cx<State>) -> El {
    let Some((i, _)) = &s.open else { return overview(s) };
    let k = Kit::neutral(true);
    let title = s.flows.get(*i).map(|f| f.title.as_str()).unwrap_or_default();
    let back = row()
        .key("back")
        .padding(8.0)
        .radius(6.0)
        .child(text("← Flows").color(k.c.text_muted))
        .on_press(|s: &mut State| s.open = None);
    let bar = row()
        .w_full()
        .padding(8.0)
        .gap(12.0)
        .items_center()
        .child(back)
        .child(text(title).font(type_::TITLE).color(k.c.text_base));
    let d = DiffView::new(
        |s: &State| match &s.open {
            Some((_, d)) => d,
            None => unreachable!("diff view built without an open flow"),
        },
        |s: &mut State| match &mut s.open {
            Some((_, d)) => d,
            None => unreachable!("diff view built without an open flow"),
        },
    )
    .build(s, &k);
    col().w_full().h_full().fill(k.c.bg_base).child(bar).child(d)
}

fn on_key(s: &mut State, ev: &KeyEvent, tx: &mut Tx) -> bool {
    match &mut s.open {
        Some(_) if ev.key == Key::Escape => {
            s.open = None;
            true
        }
        Some((_, d)) => d.key(ev, tx),
        None => false,
    }
}

fn main() {
    rune::App::new(AppSpec {
        on_key: Some(on_key),
        families: ("Inter", "ui-monospace, Inter"),
        ..AppSpec::new("throughline", (1200.0, 800.0), init, view)
    })
    .modifier_keys()
    .run();
}
