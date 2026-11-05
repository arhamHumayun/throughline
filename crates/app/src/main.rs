//! GPUI hello world wired to `core`: opens a window listing the flow groups
//! for the repo you launch it in (main...HEAD by default).
//! Dev args: diffview [repo] [base] [head]

use diffview_core::analysis::{analyze, DiffSpec};
use gpui::{
    div, prelude::*, px, rgb, size, App, Bounds, Context, SharedString, Window, WindowBounds,
    WindowOptions,
};
use gpui_platform::application;

struct Overview {
    title: SharedString,
    lines: Vec<SharedString>,
}

impl Render for Overview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .size_full()
            .bg(rgb(0x1e1e1e))
            .text_color(rgb(0xe0e0e0))
            .child(div().text_xl().child(self.title.clone()))
            .children(self.lines.iter().map(|l| div().child(l.clone())))
    }
}

fn load(args: &[String]) -> (SharedString, Vec<SharedString>) {
    let repo = args.get(1).cloned().unwrap_or_else(|| ".".into());
    let base = args.get(2).cloned().unwrap_or_else(|| "main".into());
    let head = args.get(3).cloned().unwrap_or_else(|| "HEAD".into());
    match analyze(std::path::Path::new(&repo), &DiffSpec::Refs { base: base.clone(), head: head.clone() }) {
        Ok(out) => (
            format!("{} files, {} groups ({base}...{head})", out.summary.total_files_changed, out.groups.len()).into(),
            out.groups
                .iter()
                .map(|g| format!("{}  ({} files)", g.name, g.files.len()).into())
                .collect(),
        ),
        Err(e) => ("Hello from diffview".into(), vec![format!("core: {e}").into()]),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (title, lines) = load(&args);
    application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(800.), px(600.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| Overview { title, lines }),
        )
        .expect("open window");
        cx.activate(true);
    });
}
