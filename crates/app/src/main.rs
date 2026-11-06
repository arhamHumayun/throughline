//! Overview window wired to `core`: lists the flow groups for a diff.
//! Repo and refs come from env: THROUGHLINE_REPO (default "."), THROUGHLINE_BASE ("main"), THROUGHLINE_HEAD ("HEAD").
//! `throughline` opens the window; `throughline --snap out.png` renders it headless.

use throughline_core::analysis::{DiffSpec, analyze};
use rune::tok::{self, type_};
use rune::{AppSpec, Cx, El, col, text};

struct State {
    title: String,
    lines: Vec<String>,
}

fn init() -> State {
    let var = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    let (repo, base, head) = (var("THROUGHLINE_REPO", "."), var("THROUGHLINE_BASE", "main"), var("THROUGHLINE_HEAD", "HEAD"));
    match analyze(std::path::Path::new(&repo), &DiffSpec::Refs { base: base.clone(), head: head.clone() }) {
        Ok(a) => {
            let stem = |p: &str| p.rsplit('/').next().unwrap_or(p).split('.').next().unwrap_or(p).to_string();
            let f = &a.flows;
            let mut lines: Vec<String> = f
                .flows
                .iter()
                .map(|fl| format!("{}  ({}, {} files)", fl.title, stem(&fl.root), fl.files.len()))
                .collect();
            if !f.standalone.is_empty() {
                lines.push(format!("Standalone: {}", f.standalone.iter().map(|p| stem(p)).collect::<Vec<_>>().join(", ")));
            }
            State {
                title: format!("{} files, {} flows ({base}...{head})", a.output.summary.total_files_changed, f.flows.len()),
                lines,
            }
        }
        Err(e) => State { title: "throughline".into(), lines: vec![format!("core: {e}")] },
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
    rune::run(AppSpec::<State>::new("throughline", (800.0, 600.0), init, view));
}
