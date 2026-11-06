//! throughline: three screens. Overview (a diff's flows) -> flow tree (who calls whom)
//! -> split diff of the flow's files.
//! Repo and refs come from env: THROUGHLINE_REPO (default "."), THROUGHLINE_BASE ("main"), THROUGHLINE_HEAD ("HEAD").
//! `throughline` opens the window; `throughline --snap out.png` renders it headless.
//! For headless snaps, THROUGHLINE_OPEN=<n> starts on flow n's tree (0-based), and
//! THROUGHLINE_OPEN=<n>:<f> on its diff at file f.
//!
//! Click a flow, then a file. Escape goes back one screen. In the diff, j/k move between
//! changes and ]/[ between files.

use rune::tok::type_;
use rune::{AppSpec, Cx, El, Key, KeyEvent, ScrollSpec, Tx, col, row, text};
use rune_kit::Kit;
use rune_kit::diff::{DiffMode, DiffState, DiffView, FileDiff};
use throughline_core::analysis::{DiffSpec, analyze};
use throughline_core::flows::Flow;

struct Repo {
    path: std::path::PathBuf,
    base: String,
    head: String,
}

struct Open {
    flow: usize,
    /// One diff per file in `flow_paths` order.
    diff: DiffState,
    /// Showing the diff; otherwise the tree.
    in_diff: bool,
}

struct State {
    repo: Repo,
    title: String,
    flows: Vec<Flow>,
    standalone: Vec<String>,
    error: Option<String>,
    /// `None` shows the overview.
    open: Option<Open>,
}

fn stem(p: &str) -> &str {
    let name = p.rsplit('/').next().unwrap_or(p);
    name.split('.').next().unwrap_or(name)
}

fn dir(p: &str) -> &str {
    p.rsplit_once('/').map_or("", |(d, _)| d)
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
    if let Ok(v) = std::env::var("THROUGHLINE_OPEN") {
        let mut it = v.split(':').map(|x| x.parse::<usize>().ok());
        if let Some(Some(i)) = it.next() {
            open_flow(&mut s, i);
            if let (Some(Some(f)), Some(o)) = (it.next(), s.open.as_mut()) {
                o.in_diff = true;
                o.diff.set_collapsed(f, false);
                let n = o.diff.files().len();
                for j in 0..f.min(n) {
                    o.diff.set_collapsed(j, true);
                }
            }
        }
    }
    s
}

/// Files in reading order: the call tree depth first, then the flow's tests.
fn flow_paths(f: &Flow) -> Vec<&str> {
    let mut out: Vec<&str> = f.tree.iter().filter(|n| !n.repeat).map(|n| n.file.as_str()).collect();
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
            let mut diff = DiffState::new(files);
            diff.set_mode(DiffMode::Split);
            s.open = Some(Open { flow: i, diff, in_diff: false });
        }
        Err(e) => s.error = Some(format!("loading {}: {e}", f.title)),
    }
}

fn open_file(s: &mut State, path: &str, tx: &mut Tx) {
    let Some(o) = s.open.as_mut() else { return };
    let Some(f) = o.diff.file_index(path) else { return };
    o.in_diff = true;
    o.diff.set_collapsed(f, false);
    o.diff.scroll_to_file(f, tx);
}

fn page(k: &Kit) -> El {
    col().w_full().h_full().padding(16.0).gap(12.0).fill(k.c.bg_base)
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
    let mut c = page(&k).child(text(s.title.as_str()).font(type_::TITLE).color(k.c.text_base));
    if let Some(e) = &s.error {
        c = c.child(text(e.as_str()).color(k.c.danger.fg));
    }
    c.child(list)
}

fn back(k: &Kit, label: &str) -> El {
    row()
        .key("back")
        .padding(8.0)
        .radius(6.0)
        .child(text(format!("\u{2190} {label}")).color(k.c.text_muted))
        .on_press(go_back)
}

fn go_back(s: &mut State) {
    match &mut s.open {
        Some(o) if o.in_diff => o.in_diff = false,
        _ => s.open = None,
    }
}

fn bar(k: &Kit, label: &str, title: &str) -> El {
    row()
        .w_full()
        .gap(12.0)
        .items_center()
        .child(back(k, label))
        .child(text(title.to_string()).font(type_::TITLE).color(k.c.text_base))
}

/// "+a -d" for a file in the open diff.
fn stats(o: &Open, path: &str) -> String {
    o.diff
        .file_index(path)
        .map(|i| {
            let f = &o.diff.files()[i];
            format!("+{} \u{2212}{}", f.additions(), f.deletions())
        })
        .unwrap_or_default()
}

fn file_row(k: &Kit, o: &Open, key: String, path: &str, depth: usize, via: &[String], repeat: bool) -> El {
    let p = path.to_string();
    // Tests often share their subject's stem, so show their full file name.
    let name = if throughline_core::flows::is_test_or_story(path) { path.rsplit('/').next().unwrap_or(path) } else { stem(path) };
    let mut r = row()
        .key(key)
        .padding(6.0)
        .gap(10.0)
        .radius(6.0)
        .items_center()
        .child(row().w(20.0 * depth as f32))
        .child(text(name.to_string()).color(if repeat { k.c.text_muted } else { k.c.text_base }));
    if repeat {
        r = r.child(text("(shown above)").color(k.c.text_faint));
    } else {
        r = r.child(text(stats(o, path)).color(k.c.text_muted));
    }
    // The symbol is usually the file's namesake; say so only when it isn't.
    let via: Vec<&str> = via.iter().map(String::as_str).filter(|v| *v != stem(path)).collect();
    if !via.is_empty() {
        r = r.child(text(format!("calls {}", via.join(", "))).color(k.c.text_accent));
    }
    r.child(text(dir(path).to_string()).color(k.c.text_faint))
        .on_press_tx(move |s: &mut State, tx: &mut Tx| open_file(s, &p, tx))
}

fn tree(s: &State, o: &Open) -> El {
    let k = Kit::neutral(true);
    let Some(f) = s.flows.get(o.flow) else { return overview(s) };
    let mut list = col().gap(2.0);
    for (i, n) in f.tree.iter().enumerate() {
        list = list.child(file_row(&k, o, format!("node-{i}"), &n.file, n.depth, &n.via, n.repeat));
    }
    if !f.tests.is_empty() {
        list = list.child(text("Tests").color(k.c.text_muted).padding(6.0));
        for (i, t) in f.tests.iter().enumerate() {
            list = list.child(file_row(&k, o, format!("test-{i}"), t, 0, &[], false));
        }
    }
    page(&k)
        .child(bar(&k, "Flows", &f.title))
        .child(col().key("tree").w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(list))
}

fn view(s: &State, _: &mut Cx<State>) -> El {
    let Some(o) = &s.open else { return overview(s) };
    if !o.in_diff {
        return tree(s, o);
    }
    let k = Kit::neutral(true);
    let title = s.flows.get(o.flow).map(|f| f.title.as_str()).unwrap_or_default();
    let d = DiffView::new(
        |s: &State| match &s.open {
            Some(o) => &o.diff,
            None => unreachable!("diff view built without an open flow"),
        },
        |s: &mut State| match &mut s.open {
            Some(o) => &mut o.diff,
            None => unreachable!("diff view built without an open flow"),
        },
    )
    .build(s, &k);
    col()
        .w_full()
        .h_full()
        .fill(k.c.bg_base)
        .child(row().w_full().padding(8.0).child(bar(&k, "Tree", title)))
        .child(d)
}

fn on_key(s: &mut State, ev: &KeyEvent, tx: &mut Tx) -> bool {
    if s.open.is_some() && ev.key == Key::Escape {
        go_back(s);
        return true;
    }
    match &mut s.open {
        Some(o) if o.in_diff => o.diff.key(ev, tx),
        _ => false,
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
