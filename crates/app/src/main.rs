//! Overview window wired to `core`: lists the flow groups for a diff.
//! Repo and refs come from env: DIFFVIEW_REPO (default "."), DIFFVIEW_BASE ("main"), DIFFVIEW_HEAD ("HEAD").
//! `diffview` opens the window; `diffview --snap out.png` renders it headless.

use diffview_core::analysis::{DiffSpec, analyze};
use rune::tok::{self, type_};
use rune::{AppSpec, Cx, El, col, text};

struct State {
    title: String,
    lines: Vec<String>,
}

fn init() -> State {
    let var = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    let (repo, base, head) = (var("DIFFVIEW_REPO", "."), var("DIFFVIEW_BASE", "main"), var("DIFFVIEW_HEAD", "HEAD"));
    match analyze(std::path::Path::new(&repo), &DiffSpec::Refs { base: base.clone(), head: head.clone() }) {
        Ok(a) => {
            let stem = |p: &str| p.rsplit('/').next().unwrap_or(p).split('.').next().unwrap_or(p).to_string();
            let f = &a.flows;
            let mut lines: Vec<String> = f
                .flows
                .iter()
                .map(|fl| format!("{}  ({} files, {} tests)", stem(&fl.root), fl.files.len(), fl.tests.len()))
                .collect();
            if !f.standalone.is_empty() {
                lines.push(format!("Standalone: {}", f.standalone.iter().map(|p| stem(p)).collect::<Vec<_>>().join(", ")));
            }
            State {
                title: format!("{} files, {} flows ({base}...{head})", a.output.summary.total_files_changed, f.flows.len()),
                lines,
            }
        }
        Err(e) => State { title: "diffview".into(), lines: vec![format!("core: {e}")] },
    }
}

fn view(s: &State, _: &mut Cx<State>) -> El {
    let t = tok::theme(true);
    let mut list = col().gap(8.0);
    for l in &s.lines {
        list = list.child(text(l.as_str()).color(t.text));
    }
    col()
        .w_full()
        .h_full()
        .padding(16.0)
        .gap(12.0)
        .fill(t.bg)
        .child(text(s.title.as_str()).font(type_::TITLE).color(t.text))
        .child(list)
}

fn main() {
    rune::run(AppSpec::<State>::new("diffview", (800.0, 600.0), init, view));
}
