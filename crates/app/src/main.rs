//! throughline: open a local repo -> choose what to compare -> overview (the diff's flows)
//! -> flow tree (who calls whom, and what changed in each file) -> one function, or the
//! flow's whole diff.
//!
//! The first screen opens a repo: a recent one, a folder from the system picker, a pasted
//! path, or a folder dropped on the window. The second picks a base branch and what to compare
//! it with: another local branch, or the working tree (commits plus uncommitted edits). The diff
//! is what that side added since it forked from the base (`git diff base...head`).
//!
//! `throughline` opens the window; `throughline --snap out.png` renders it headless.
//! Dev shortcut, mostly for headless snaps: THROUGHLINE_REPO=<path> skips the first screen, and
//! with THROUGHLINE_BASE and THROUGHLINE_HEAD (a branch, or `worktree`) also the second.
//! Then THROUGHLINE_OPEN=<n> starts on flow n's tree (0-based), and THROUGHLINE_OPEN=<n>:<f> on
//! its diff at file f. THROUGHLINE_CONFIG_DIR overrides where the recent-repos list lives.
//!
//! Click a flow, then a file (its diff in the flow's diff) or one of its changed functions
//! (just that function, with links to the changed functions it calls and that call it).
//! Escape goes back one screen. In a diff, j/k move between changes and ]/[ between files.

use std::path::{Path, PathBuf};

use rune::tok::type_;
use rune::{AppSpec, Cx, El, FileDialogOptions, Key, KeyEvent, Latest, ScrollSpec, TextEdit, Tx, Waker, col, row, text};
use rune_kit::diff::{DiffMode, DiffState, DiffView, FileDiff};
use rune_kit::{Button, ButtonVariant, Kit, TextInput};
use throughline_core::analysis::{Analysis, DiffSpec, analyze};
use throughline_core::flows::{Flow, is_test_or_story};
use throughline_core::functions::{FnChange, Status, changed_functions};
use throughline_core::local::{self, LocalRepo, Recent};
use throughline_core::types::SymbolKind;

/// What to compare the base with.
#[derive(Clone, Debug, PartialEq)]
enum Head {
    /// The checkout as it is on disk: its commits plus uncommitted and untracked files.
    Worktree,
    Branch(String),
}

impl Head {
    fn label(&self) -> &str {
        match self {
            Head::Worktree => "working tree",
            Head::Branch(b) => b,
        }
    }
}

/// The comparison the overview and everything after it show.
struct Repo {
    root: PathBuf,
    /// The commit the old side is read from (the merge base).
    base_sha: String,
    /// The commit the new side is read from; `None` reads the working tree.
    head_sha: Option<String>,
}

/// The repo chosen on the first screen, and the choice being made on the second.
struct Pick {
    repo: LocalRepo,
    base: Option<String>,
    head: Head,
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
    recent: Recent,
    recent_file: Option<PathBuf>,
    /// The first screen's path field.
    path: TextEdit,
    /// `None` shows the first screen (open a repo).
    pick: Option<Pick>,
    /// The comparison running off the main thread: the root it ran in and its answer.
    analysis: Latest<(PathBuf, Head, Result<Analysis, String>)>,
    /// `None` shows the second screen (choose what to compare), once a repo is picked.
    repo: Option<Repo>,
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

/// Where the recent-repos list lives: %APPDATA%\throughline on Windows, else
/// $XDG_CONFIG_HOME/throughline or ~/.config/throughline.
fn recent_file() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let dir = env("THROUGHLINE_CONFIG_DIR").or_else(|| {
        let base = if cfg!(windows) {
            env("APPDATA")
        } else {
            env("XDG_CONFIG_HOME").or_else(|| env("HOME").map(|h| h.join(".config")))
        };
        base.map(|b| b.join("throughline"))
    })?;
    Some(dir.join("recent.txt"))
}

fn init() -> State {
    let recent_file = recent_file();
    let mut s = State {
        recent: recent_file.as_deref().map(Recent::load).unwrap_or_default(),
        recent_file,
        path: TextEdit::new(""),
        pick: None,
        analysis: Latest::default(),
        repo: None,
        title: String::new(),
        flows: vec![],
        standalone: vec![],
        error: None,
        open: None,
        diff: DiffState::new(vec![]),
        func_diff: DiffState::new(vec![]),
    };
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    if let Some(path) = var("THROUGHLINE_REPO") {
        open_repo(&mut s, Path::new(&path));
        if let (Some(p), Some(base), Some(head)) = (s.pick.as_mut(), var("THROUGHLINE_BASE"), var("THROUGHLINE_HEAD")) {
            p.base = Some(base);
            p.head = if head == "worktree" { Head::Worktree } else { Head::Branch(head) };
            let (root, head) = (p.repo.root.clone(), p.head.clone());
            let a = run_analysis(&root, p.base.as_deref().unwrap_or_default(), &head);
            take_analysis(&mut s, root, head, a);
        }
    }
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

/// The usual first choice: the default branch against your branch, or against the working
/// tree when it has uncommitted edits (your agent's output before you commit it).
fn default_pick(repo: LocalRepo) -> Pick {
    let base = repo.default_branch.clone();
    let other = repo.branches.iter().find(|b| Some(*b) != base.as_ref()).cloned();
    let head = match (&repo.current, repo.dirty) {
        (_, true) => Head::Worktree,
        (Some(c), false) if Some(c) != base.as_ref() => Head::Branch(c.clone()),
        _ => other.map_or(Head::Worktree, Head::Branch),
    };
    Pick { repo, base, head }
}

/// First screen -> second: open the repository containing `path`.
fn open_repo(s: &mut State, path: &Path) {
    match local::open(path) {
        Ok(r) => {
            s.recent.push(&r.root);
            if let Some(f) = &s.recent_file {
                // Losing the recents list isn't worth failing the open over.
                let _ = s.recent.save(f);
            }
            s.error = None;
            s.pick = Some(default_pick(r));
        }
        Err(e) => s.error = Some(e.to_string()),
    }
}

fn run_analysis(root: &Path, base: &str, head: &Head) -> Result<Analysis, String> {
    let spec = match head {
        Head::Worktree => DiffSpec::BranchToWorkdir { base: base.to_string() },
        Head::Branch(h) => DiffSpec::Refs { base: base.to_string(), head: h.clone() },
    };
    analyze(root, &spec).map_err(|e| e.to_string())
}

/// Second screen -> overview: start the comparison. Headless runs analyze in place, so a
/// snapshot shows the result instead of the wait.
fn compare(s: &mut State) {
    let Some(p) = &s.pick else { return };
    let Some(base) = p.base.clone() else {
        s.error = Some("Pick a base branch".into());
        return;
    };
    if p.head == Head::Branch(base.clone()) {
        s.error = Some(format!("{base} compared with itself is empty; pick another branch or the working tree"));
        return;
    }
    let (root, head) = (p.repo.root.clone(), p.head.clone());
    s.error = None;
    if rune::tooling::headless_run().is_some() {
        let a = run_analysis(&root, &base, &head);
        take_analysis(s, root, head, a);
    } else {
        s.analysis.request((root, head, base), |(root, head, base)| {
            let a = run_analysis(&root, &base, &head);
            (root, head, a)
        });
    }
}

fn take_analysis(s: &mut State, root: PathBuf, head: Head, a: Result<Analysis, String>) {
    // Ignore an answer for a repo or choice the user has since left.
    let Some(p) = &s.pick else { return };
    if p.repo.root != root || p.head != head {
        return;
    }
    let base = p.base.clone().unwrap_or_default();
    match a {
        Ok(a) => {
            s.title = format!(
                "{} files, {} flows ({base}...{})",
                a.output.summary.total_files_changed,
                a.flows.flows.len(),
                head.label()
            );
            s.flows = a.flows.flows;
            s.standalone = a.flows.standalone;
            s.error = None;
            s.open = None;
            s.repo = Some(Repo { root, base_sha: a.base_sha.unwrap_or(base), head_sha: a.head_sha });
        }
        Err(e) => s.error = Some(e),
    }
}

fn poll(s: &mut State, _: f64, _: &mut Tx) -> bool {
    let Some((root, head, a)) = s.analysis.take() else { return false };
    take_analysis(s, root, head, a);
    true
}

fn setup(s: &mut State, waker: Waker) {
    s.analysis.set_waker(waker);
}

/// Files in reading order: the call tree depth first, then the flow's tests.
fn flow_paths(f: &Flow) -> Vec<&str> {
    let mut out: Vec<&str> = f.tree.iter().filter(|n| !n.repeat).map(|n| n.file.as_str()).collect();
    out.extend(f.tests.iter().map(String::as_str));
    out
}

/// Base and head text of each file. Added files have an empty base, deleted an empty head.
fn load_texts(repo: &Repo, paths: &[&str]) -> Result<Vec<(String, String)>, String> {
    let r = git2::Repository::open(&repo.root).map_err(|e| e.to_string())?;
    let at = |rev: &str, p: &str| throughline_core::git::file_content_at_ref(&r, rev, p).map_err(|e| e.to_string());
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        let old = at(&repo.base_sha, p)?.unwrap_or_default();
        let new = match &repo.head_sha {
            Some(h) => at(h, p)?,
            None => local::workdir_text(&repo.root, p),
        };
        out.push((old, new.unwrap_or_default()));
    }
    Ok(out)
}

fn open_flow(s: &mut State, i: usize) {
    let (Some(f), Some(repo)) = (s.flows.get(i), &s.repo) else { return };
    let paths = flow_paths(f);
    match load_texts(repo, &paths) {
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
    if s.flows.is_empty() && s.standalone.is_empty() {
        list = list.child(text("No code changes to show.").color(k.c.text_muted).padding(8.0));
    }
    let name = s.pick.as_ref().map_or("Compare", |p| p.repo.name.as_str());
    let mut c = page(&k).child(bar(&k, name, &s.title));
    if let Some(e) = &s.error {
        c = c.child(text(e.as_str()).color(k.c.danger.fg));
    }
    c.child(col().key("flows").w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(list))
}

/// Screen 0: open a repository. Recent repos, the system folder picker, a pasted path, or a
/// folder dropped on the window.
fn open_screen(s: &State) -> El {
    let k = Kit::neutral(true);
    let field = TextInput::new(&s.path)
        .placeholder("Paste a repository path and press Enter")
        .on_edit(|s: &mut State| &mut s.path)
        .on_submit(|s: &mut State, _: &mut Tx| {
            let p = s.path.text.trim().to_string();
            if !p.is_empty() {
                open_repo(s, Path::new(&p));
            }
        })
        .build(&k);
    let browse = Button::new("Choose folder\u{2026}")
        .variant(ButtonVariant::Contrast)
        .on_press_tx(|_: &mut State, tx: &mut Tx| {
            let opts = FileDialogOptions { title: "Open a git repository".into(), ..Default::default() };
            tx.open_folder(opts, |s: &mut State, r, _| match r {
                Ok(Some(p)) => open_repo(s, &p),
                Ok(None) => {}
                Err(e) => s.error = Some(format!("folder picker: {e}")),
            });
        })
        .build(&k)
        .key("browse");
    let mut c = page(&k)
        .child(text("Open a repository").font(type_::TITLE).color(k.c.text_base))
        .child(row().w_full().gap(8.0).items_center().child(browse).child(col().grow(1.0).child(field)));
    if let Some(e) = &s.error {
        c = c.child(text(e.as_str()).color(k.c.danger.fg));
    }
    let mut list = col().gap(2.0);
    for (i, p) in s.recent.existing().enumerate() {
        let name = p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned());
        let path = p.clone();
        list = list.child(
            row()
                .key(format!("recent-{i}"))
                .padding(8.0)
                .gap(12.0)
                .radius(6.0)
                .items_center()
                .child(text(name).color(k.c.text_base))
                .child(text(p.display().to_string()).color(k.c.text_faint))
                .on_press(move |s: &mut State| open_repo(s, &path)),
        );
    }
    if s.recent.existing().next().is_some() {
        c = c.child(text("Recent").color(k.c.text_muted).px(8.0)).child(list);
    } else {
        c = c.child(text("Or drop a repository folder here.").color(k.c.text_faint).px(8.0));
    }
    c.on_drop_files(|s: &mut State, files: &[PathBuf], _: &mut Tx| {
        if let Some(p) = files.first() {
            open_repo(s, p);
        }
    })
    .accepts_files(|p| p.is_dir())
}

fn choice(k: &Kit, key: String, label: &str, note: &str, on: bool) -> El {
    let mut r = row().key(key).px(8.0).py(5.0).gap(10.0).radius(6.0).items_center();
    if on {
        r = r.fill(k.c.layer_03);
    }
    r = r.child(text(label.to_string()).color(if on { k.c.text_base } else { k.c.text_muted }));
    if !note.is_empty() {
        r = r.child(text(note.to_string()).color(k.c.text_faint));
    }
    r
}

/// Screen 1: choose what to compare. A base branch on the left, and on the right what it's
/// compared with: the working tree or another branch.
fn compare_screen(s: &State, p: &Pick) -> El {
    let k = Kit::neutral(true);
    let current = p.repo.current.as_deref();
    let note = |b: &str| if Some(b) == current { "checked out" } else { "" };
    let mut bases = col().gap(2.0);
    for (i, b) in p.repo.branches.iter().enumerate() {
        let name = b.clone();
        bases = bases.child(
            choice(&k, format!("base-{i}"), b, note(b), p.base.as_deref() == Some(b))
                .on_press(move |s: &mut State| if let Some(p) = s.pick.as_mut() { p.base = Some(name.clone()) }),
        );
    }
    let wt_note = if p.repo.dirty { "your branch plus uncommitted changes" } else { "no uncommitted changes" };
    let mut heads = col().gap(2.0).child(
        choice(&k, "head-worktree".into(), "Working tree", wt_note, p.head == Head::Worktree)
            .on_press(|s: &mut State| if let Some(p) = s.pick.as_mut() { p.head = Head::Worktree }),
    );
    for (i, b) in p.repo.branches.iter().enumerate() {
        let h = Head::Branch(b.clone());
        let on = p.head == h;
        heads = heads.child(
            choice(&k, format!("head-{i}"), b, note(b), on).on_press(move |s: &mut State| if let Some(p) = s.pick.as_mut() { p.head = h.clone() }),
        );
    }
    let column = |label: &str, key: &str, list: El| {
        col()
            .grow(1.0)
            .w(0.0)
            .min_h(0.0)
            .gap(6.0)
            .child(text(label.to_string()).color(k.c.text_muted).px(8.0))
            .child(col().key(key.to_string()).w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(list))
    };
    let base = p.base.as_deref().unwrap_or("?");
    let busy = s.analysis.is_pending();
    let go = Button::new(if busy { "Comparing\u{2026}".to_string() } else { format!("Compare {base} with {}", p.head.label()) })
        .variant(if busy { ButtonVariant::Loading } else { ButtonVariant::Contrast })
        .disabled(busy)
        .on_press(compare)
        .build(&k)
        .key("compare");
    let mut c = page(&k).child(bar(&k, "Repositories", &p.repo.name)).child(
        row().w_full().gap(12.0).items_center().child(go).child(text(p.repo.root.display().to_string()).color(k.c.text_faint)),
    );
    if let Some(e) = &s.error {
        c = c.child(text(e.as_str()).color(k.c.danger.fg));
    }
    c.child(
        row()
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .gap(16.0)
            .child(column("Base", "bases", bases))
            .child(column("Compare with", "heads", heads)),
    )
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
    s.error = None;
    match &mut s.open {
        Some(o) if !matches!(o.screen, Screen::Tree) => o.screen = Screen::Tree,
        Some(_) => s.open = None,
        None if s.repo.is_some() => s.repo = None,
        None => {
            s.analysis.cancel();
            s.pick = None;
        }
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
    let Some(p) = &s.pick else { return open_screen(s) };
    if s.repo.is_none() {
        return compare_screen(s, p);
    }
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
    if s.pick.is_some() && ev.key == Key::Escape {
        go_back(s);
        return true;
    }
    if s.pick.is_some() && s.repo.is_none() && ev.key == Key::Enter && !s.analysis.is_pending() {
        compare(s);
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
        setup: Some(setup),
        poll: Some(poll),
        families: ("Inter", "ui-monospace, Inter"),
        ..AppSpec::new("throughline", (1200.0, 800.0), init, view)
    })
    .modifier_keys()
    .run();
}
