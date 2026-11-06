//! Milestone 1 benchmark: rune-kit's split `DiffView` on large diffs.
//!
//! BENCH=synth  (default) a 10,000-line TS-like file with every 25th line edited (~400 hunks)
//! BENCH=<path> a real file, with every 100th line edited
//! `cargo run --release -p throughline-app --example diff_bench -- --snap out.png`
//! `... --script crates/app/examples/scroll_10k.json --snap out.png --at mark:end`
#![allow(clippy::print_stderr)]

use rune::{AppSpec, Cx, El, KeyEvent, Tx, col};
use rune_kit::Kit;
use rune_kit::diff::{DiffMode, DiffState, DiffView, FileDiff};

struct S {
    diff: DiffState,
}

fn synth(n: usize) -> String {
    (0..n)
        .map(|i| match i % 7 {
            0 => format!("export function handler{i}(input: Input{i}): Output {{"),
            1 => format!("  const value{i} = compute(input.field{i}, {i});"),
            2 => format!("  if (value{i} > LIMIT) return fallback(\"case {i}\");"),
            3 => format!("  log.debug(`step {i}`, {{ value: value{i} }});"),
            4 => format!("  return transform(value{i}) as Output;"),
            5 => "}".to_string(),
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn mutate(src: &str, every: usize) -> String {
    src.lines()
        .enumerate()
        .flat_map(|(i, l)| {
            if i % every == every - 1 {
                vec![format!("{l} // changed"), format!("  // inserted after line {i}")]
            } else {
                vec![l.to_string()]
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn init() -> S {
    let which = std::env::var("BENCH").unwrap_or_else(|_| "synth".into());
    let (path, old, new) = if which == "synth" {
        let n: usize = std::env::var("LINES").ok().and_then(|s| s.parse().ok()).unwrap_or(10_000);
        let old = synth(n);
        let new = mutate(&old, 25);
        ("synth.ts".to_string(), old, new)
    } else {
        let old = std::fs::read_to_string(&which).unwrap_or_default();
        let new = mutate(&old, 100);
        (which.clone(), old, new)
    };
    let t = std::time::Instant::now();
    let file = FileDiff::from_texts(&path, &old, &new);
    let mut diff = DiffState::new(vec![file]);
    diff.set_mode(DiffMode::Split);
    eprintln!(
        "bench: {} old lines, {} new lines, diff+state {:.1} ms",
        old.lines().count(),
        new.lines().count(),
        t.elapsed().as_secs_f64() * 1000.0
    );
    S { diff }
}

fn view(s: &S, cx: &mut Cx<S>) -> El {
    let k = Kit::neutral(cx.system_dark());
    let d = DiffView::new(|s: &S| &s.diff, |s: &mut S| &mut s.diff).build(s, &k);
    col().w_full().h_full().fill(k.c.bg_base).child(d)
}

fn on_key(s: &mut S, ev: &KeyEvent, tx: &mut Tx) -> bool {
    s.diff.key(ev, tx)
}

fn main() {
    rune::App::new(AppSpec {
        on_key: Some(on_key),
        families: ("Inter", "ui-monospace, Inter"),
        ..AppSpec::new("diff bench", (1200.0, 800.0), init, view)
    })
    .modifier_keys()
    .run();
}
