//! throughline: overview (a diff's flows) -> flow tree (who calls whom, and what
//! changed in each file) -> one function, or the flow's whole diff.
//! Repo and refs come from env: THROUGHLINE_REPO (default "."), THROUGHLINE_BASE ("main"), THROUGHLINE_HEAD ("HEAD").
//! `throughline` opens the window; `throughline --snap out.png` renders it headless.
//! For headless snaps, THROUGHLINE_OPEN=<n> starts on flow n's tree (0-based), and
//! THROUGHLINE_OPEN=<n>:<f> on its diff at file f.
//!
//! Click a flow, then a file (its diff in the flow's diff) or one of its changed functions
//! (just that function, with links to the changed functions it calls and that call it).
//! Escape goes back one screen. In a diff, j/k move between changes and ]/[ between files.

use rune::tok::type_;
use rune::{AppSpec, Cx, El, Key, KeyEvent, ScrollSpec, Tx, col, row, text};
use rune_kit::Kit;
use rune_kit::diff::{DiffMode, DiffState, DiffView, FileDiff};
use throughline_core::analysis::{DiffSpec, analyze};
use throughline_core::flows::{Flow, is_test_or_story};
use throughline_core::types::SymbolKind;
use throughline_core::functions::{FnChange, Status, changed_functions};

struct Repo {
    path: std::path::PathBuf,
    base: String,
    head: String,
}

enum Screen {
    Tree,
    /// The whole flow's diff.
    Diff,
    /// One changed function: file index and function index; its diff is `State::func_diff`.
    Func(usize, usize),
}

struct Open {
    flow: usize,
    /// Base and head text of each file, same order.
    texts: Vec<(String, String)>,
    /// Changed functions of each file, same order.
    fns: Vec<Vec<FnChange>>,
    screen: Screen,
}

struct State {
    repo: Repo,
    title: String,
    flows: Vec<Flow>,
    standalone: Vec<String>,
    error: Option<String>,
    /// `None` shows the overview.
    open: Option<Open>,
    /// The open flow's files, one per file in `flow_paths` order. Diff states live here, not
    /// in `Open`, so they outlive screen changes: the view caches rows by state revision, and
    /// its handlers may still reach for the state after the screen changes.
    diff: DiffState,
    /// The open function's diff.
    func_diff: DiffState,
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
            diff: DiffState::new(vec![]),
            func_diff: DiffState::new(vec![]),
            repo,
        },
        Err(e) => State {
            title: "throughline".into(),
            flows: vec![],
            standalone: vec![],
            error: Some(format!("core: {e}")),
            open: None,
            diff: DiffState::new(vec![]),
            func_diff: DiffState::new(vec![]),
            repo,
        },
    };
    if let Ok(v) = std::env::var("THROUGHLINE_OPEN") {
        let mut it = v.split(':').map(|x| x.parse::<usize>().ok());
        if let Some(Some(i)) = it.next() {
            open_flow(&mut s, i);
            if let (Some(Some(f)), Some(o)) = (it.next(), s.open.as_mut()) {
                o.screen = Screen::Diff;
                s.diff.set_collapsed(f, false);
                let n = s.diff.files().len();
                for j in 0..f.min(n) {
                    s.diff.set_collapsed(j, true);
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

/// Base and head text of each file. Added files have an empty base, deleted an empty head.
fn load_texts(repo: &Repo, paths: &[&str]) -> Result<Vec<(String, String)>, String> {
    let r = git2::Repository::open(&repo.path).map_err(|e| e.to_string())?;
    let at = |rev: &str, p: &str| throughline_core::git::file_content_at_ref(&r, rev, p).map_err(|e| e.to_string());
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        let old = at(&repo.base, p)?.unwrap_or_default();
        let new = at(&repo.head, p)?.unwrap_or_default();
        out.push((old, new));
    }
    Ok(out)
}

fn open_flow(s: &mut State, i: usize) {
    let Some(f) = s.flows.get(i) else { return };
    let paths = flow_paths(f);
    match load_texts(&s.repo, &paths) {
        Ok(texts) => {
            let files = paths.iter().zip(&texts).map(|(p, (o, n))| FileDiff::from_texts(*p, o, n)).collect();
            let fns = paths
                .iter()
                .zip(&texts)
                .map(|(p, (o, n))| if is_test_or_story(p) { vec![] } else { changed_functions(p, o, n) })
                .collect();
            s.diff.set_files(files);
            s.diff.set_mode(DiffMode::Split);
            s.open = Some(Open { flow: i, texts, fns, screen: Screen::Tree });
        }
        Err(e) => s.error = Some(format!("loading {}: {e}", f.title)),
    }
}

/// Lines `a..=b` (1-based) of `src`, or nothing.
fn slice(src: &str, r: Option<(usize, usize)>) -> String {
    let Some((a, b)) = r else { return String::new() };
    // Each line keeps its newline, so the slice doesn't read as "no newline at end of file".
    src.lines().skip(a.saturating_sub(1)).take(b + 1 - a.max(1)).map(|l| format!("{l}\n")).collect()
}

fn open_func(s: &mut State, file: usize, idx: usize) {
    let Some(o) = s.open.as_mut() else { return };
    let (Some((old, new)), Some(c)) = (o.texts.get(file), o.fns.get(file).and_then(|v| v.get(idx))) else { return };
    let path = s.diff.files()[file].path().to_string();
    let fd = FileDiff::from_texts(path, &slice(old, c.old), &slice(new, c.new));
    // A new or removed function has one side only; side by side would leave half the screen empty.
    let mode = if c.status == Status::Modified { DiffMode::Split } else { DiffMode::Unified };
    s.func_diff.set_files(vec![fd]);
    s.func_diff.set_mode(mode);
    o.screen = Screen::Func(file, idx);
}

/// Find a changed function by name in the open flow.
fn find_func(o: &Open, name: &str) -> Option<(usize, usize)> {
    o.fns.iter().enumerate().find_map(|(f, v)| v.iter().position(|c| c.name == name).map(|i| (f, i)))
}

fn open_file(s: &mut State, path: &str, tx: &mut Tx) {
    let Some(o) = s.open.as_mut() else { return };
    let Some(f) = s.diff.file_index(path) else { return };
    o.screen = Screen::Diff;
    s.diff.set_collapsed(f, false);
    s.diff.scroll_to_file(f, tx);
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
        Some(o) if !matches!(o.screen, Screen::Tree) => o.screen = Screen::Tree,
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
fn stats(d: &DiffState, path: &str) -> String {
    d.file_index(path)
        .map(|i| {
            let f = &d.files()[i];
            format!("+{} \u{2212}{}", f.additions(), f.deletions())
        })
        .unwrap_or_default()
}

fn file_row(k: &Kit, d: &DiffState, key: String, path: &str, depth: usize, via: &[String], repeat: bool) -> El {
    let p = path.to_string();
    // Tests often share their subject's stem, so show their full file name.
    let name = if is_test_or_story(path) { path.rsplit('/').next().unwrap_or(path) } else { stem(path) };
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
        r = r.child(text(stats(d, path)).color(k.c.text_muted));
    }
    // The symbol is usually the file's namesake; say so only when it isn't.
    let via: Vec<&str> = via.iter().map(String::as_str).filter(|v| *v != stem(path)).collect();
    if !via.is_empty() {
        r = r.child(text(format!("calls {}", via.join(", "))).color(k.c.text_accent));
    }
    r.child(text(dir(path).to_string()).color(k.c.text_faint))
        .on_press_tx(move |s: &mut State, tx: &mut Tx| open_file(s, &p, tx))
}

/// `name()` for functions, bare `name` for types and constants.
fn display(c: &FnChange) -> String {
    if c.kind == SymbolKind::Function { format!("{}()", c.name) } else { c.name.clone() }
}

fn status_label(st: Status) -> &'static str {
    match st {
        Status::Added => "new",
        Status::Modified => "edited",
        Status::Removed => "removed",
    }
}

fn func_row(k: &Kit, key: String, c: &FnChange, depth: usize) -> El {
    let color = match c.status {
        Status::Added => k.c.success.icon,
        Status::Modified => k.c.text_muted,
        Status::Removed => k.c.danger.icon,
    };
    row()
        .key(key)
        .px(6.0).py(3.0)
        .gap(10.0)
        .radius(6.0)
        .items_center()
        .child(row().w(20.0 * depth as f32 + 4.0))
        .child(text(display(c)).font(type_::MONO_CAPTION).color(k.c.text_accent))
        .child(text(status_label(c.status)).color(color))
}

/// Screen 3: one changed function, with the changed functions it calls and that call it.
fn func(s: &State, o: &Open, file: usize, idx: usize) -> El {
    let k = Kit::neutral(true);
    let Some(c) = o.fns.get(file).and_then(|v| v.get(idx)) else { return tree(s, o) };
    let path = s.diff.files()[file].path();
    let lines = match (c.old, c.new) {
        (_, Some((a, b))) | (Some((a, b)), None) => format!("lines {a}\u{2013}{b}"),
        _ => String::new(),
    };
    let mut links = col().w_full().gap(2.0).px(16.0);
    let calls: Vec<(String, usize, usize)> =
        c.calls.iter().filter_map(|n| find_func(o, n).filter(|&p| p != (file, idx)).map(|(f, i)| (display(&o.fns[f][i]), f, i))).collect();
    let callers: Vec<(String, usize, usize)> = o
        .fns
        .iter()
        .enumerate()
        .flat_map(|(f, v)| v.iter().enumerate().map(move |(i, x)| (f, i, x)))
        .filter(|(f, i, x)| (*f, *i) != (file, idx) && x.calls.contains(&c.name))
        .map(|(f, i, x)| (display(x), f, i))
        .collect();
    for (label, list) in [("Called by", callers), ("Calls", calls)] {
        let mut r = row().gap(8.0).items_center().flex_wrap().child(text(label).color(k.c.text_muted));
        if list.is_empty() {
            r = r.child(text("no changed functions").color(k.c.text_faint));
        }
        for (n, f, i) in list {
            r = r.child(
                row()
                    .key(format!("link-{n}"))
                    .px(6.0).py(3.0)
                    .radius(6.0)
                    .child(text(n).color(k.c.text_accent))
                    .on_press(move |s: &mut State| open_func(s, f, i)),
            );
        }
        links = links.child(r);
    }
    let view = DiffView::new(
        |s: &State| &s.func_diff,
        |s: &mut State| &mut s.func_diff,
    )
    .build(s, &k);
    col()
        .w_full()
        .h_full()
        .gap(8.0)
        .fill(k.c.bg_base)
        .child(row().w_full().padding(8.0).child(bar(&k, "Tree", &format!("{}  {}", display(c), status_label(c.status)))))
        .child(text(format!("{path}  {lines}")).color(k.c.text_faint).px(16.0))
        .child(links)
        .child(view)
}

fn tree(s: &State, o: &Open) -> El {
    let k = Kit::neutral(true);
    let Some(f) = s.flows.get(o.flow) else { return overview(s) };
    let mut list = col().gap(2.0);
    for (i, n) in f.tree.iter().enumerate() {
        list = list.child(file_row(&k, &s.diff, format!("node-{i}"), &n.file, n.depth, &n.via, n.repeat));
        let Some(fi) = s.diff.file_index(&n.file).filter(|_| !n.repeat) else { continue };
        // A file whose only change is its namesake needs no second line.
        if let [only] = o.fns[fi].as_slice() {
            if only.name == stem(&n.file) {
                continue;
            }
        }
        for (j, c) in o.fns[fi].iter().enumerate() {
            list = list.child(func_row(&k, format!("fn-{fi}-{j}"), c, n.depth + 1).on_press(move |s: &mut State| open_func(s, fi, j)));
        }
    }
    if !f.tests.is_empty() {
        list = list.child(text("Tests").color(k.c.text_muted).padding(6.0));
        for (i, t) in f.tests.iter().enumerate() {
            list = list.child(file_row(&k, &s.diff, format!("test-{i}"), t, 0, &[], false));
        }
    }
    page(&k)
        .child(bar(&k, "Flows", &f.title))
        .child(col().key("tree").w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(list))
}

fn view(s: &State, _: &mut Cx<State>) -> El {
    let Some(o) = &s.open else { return overview(s) };
    match &o.screen {
        Screen::Tree => return tree(s, o),
        Screen::Func(f, i) => return func(s, o, *f, *i),
        Screen::Diff => {}
    }
    let k = Kit::neutral(true);
    let title = s.flows.get(o.flow).map(|f| f.title.as_str()).unwrap_or_default();
    let d = DiffView::new(
        |s: &State| &s.diff,
        |s: &mut State| &mut s.diff,
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
        Some(Open { screen: Screen::Diff, .. }) => s.diff.key(ev, tx),
        Some(Open { screen: Screen::Func(..), .. }) => s.func_diff.key(ev, tx),
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
