//! Which functions (and other top-level symbols) a file's change touches.
//!
//! Both sides are parsed; a symbol counts as changed when its text differs, so a function
//! that only moved doesn't. When a changed symbol sits inside another changed one (a method
//! in a class), only the inner one is reported.

use serde::Serialize;

use crate::ast::{Definition, ParsedFile, parse_file};
use crate::types::SymbolKind;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum Status {
    Added,
    Modified,
    Removed,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FnChange {
    pub name: String,
    pub kind: SymbolKind,
    pub status: Status,
    /// 1-based inclusive line range in the base file, if it existed there.
    pub old: Option<(usize, usize)>,
    /// 1-based inclusive line range in the head file, if it exists there.
    pub new: Option<(usize, usize)>,
    /// Names this symbol calls or renders as JSX (last segment of each callee), in first-use order.
    pub calls: Vec<String>,
}

fn lines(src: &str, (a, b): (usize, usize)) -> String {
    src.lines().skip(a.saturating_sub(1)).take(b + 1 - a.max(1)).collect::<Vec<_>>().join("\n")
}

fn range(d: &Definition) -> (usize, usize) {
    (d.start_line, d.end_line.max(d.start_line))
}

/// `<Name` component tags on each line of `src` within `a..=b`, as (line, name).
fn jsx_tags(src: &str, (a, b): (usize, usize)) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, line) in src.lines().enumerate().skip(a.saturating_sub(1)).take(b + 1 - a.max(1)) {
        let mut rest = line;
        while let Some(p) = rest.find('<') {
            rest = &rest[p + 1..];
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.').collect();
            if name.starts_with(|c: char| c.is_ascii_uppercase()) {
                out.push((i + 1, name));
            }
        }
    }
    out
}

fn calls_in(file: &ParsedFile, src: &str, r: (usize, usize)) -> Vec<String> {
    let mut sites: Vec<(usize, String)> = file
        .call_sites
        .iter()
        .filter(|c| c.line >= r.0 && c.line <= r.1)
        .map(|c| (c.line, c.callee.clone()))
        .collect();
    if file.path.ends_with("x") {
        sites.extend(jsx_tags(src, r));
    }
    sites.sort_by_key(|(l, _)| *l);
    let mut out: Vec<String> = Vec::new();
    for (_, callee) in sites {
        let name = callee.rsplit(['.', ':']).next().unwrap_or(&callee).to_string();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// The changed symbols of one file, in head order (removed ones after, in base order).
/// Either side may be empty (an added or deleted file). Unparseable sides count as empty.
pub fn changed_functions(path: &str, old: &str, new: &str) -> Vec<FnChange> {
    let parse = |src: &str| {
        if src.is_empty() {
            None
        } else {
            parse_file(path, src).ok()
        }
    };
    let (po, pn) = (parse(old), parse(new));
    let defs = |p: &Option<ParsedFile>| -> Vec<Definition> {
        p.as_ref()
            .map(|p| p.definitions.iter().filter(|d| d.kind != SymbolKind::Module).cloned().collect())
            .unwrap_or_default()
    };
    let (od, nd) = (defs(&po), defs(&pn));

    let mut out = Vec::new();
    for n in &nd {
        let o = od.iter().find(|o| o.name == n.name && o.kind == n.kind);
        let status = match o {
            None => Status::Added,
            Some(o) if lines(old, range(o)) != lines(new, range(n)) => Status::Modified,
            Some(_) => continue,
        };
        out.push(FnChange {
            name: n.name.clone(),
            kind: n.kind.clone(),
            status,
            old: o.map(range),
            new: Some(range(n)),
            calls: pn.as_ref().map(|p| calls_in(p, new, range(n))).unwrap_or_default(),
        });
    }
    for o in &od {
        if !nd.iter().any(|n| n.name == o.name && n.kind == o.kind) {
            out.push(FnChange {
                name: o.name.clone(),
                kind: o.kind.clone(),
                status: Status::Removed,
                old: Some(range(o)),
                new: None,
                calls: po.as_ref().map(|p| calls_in(p, old, range(o))).unwrap_or_default(),
            });
        }
    }

    // Drop containers of other changes: keep the innermost.
    let contains = |a: Option<(usize, usize)>, b: Option<(usize, usize)>| match (a, b) {
        (Some(a), Some(b)) => a.0 <= b.0 && b.1 <= a.1 && a != b,
        _ => false,
    };
    let inner: Vec<bool> = out
        .iter()
        .map(|a| !out.iter().any(|b| contains(a.new, b.new) || (a.new.is_none() && contains(a.old, b.old))))
        .collect();
    out.into_iter().zip(inner).filter(|(_, keep)| *keep).map(|(c, _)| c).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn names(v: &[FnChange]) -> Vec<(&str, Status)> {
        v.iter().map(|c| (c.name.as_str(), c.status)).collect()
    }

    #[test]
    fn edited_added_and_removed_functions() {
        let old = "function keep() {\n  return 1;\n}\n\nfunction edit() {\n  return 2;\n}\n\nfunction gone() {\n  return 3;\n}\n";
        let new = "// moved down a line\nfunction keep() {\n  return 1;\n}\n\nfunction edit() {\n  return helper(2);\n}\n\nfunction fresh() {\n  return edit();\n}\n";
        let c = changed_functions("a.ts", old, new);
        assert_eq!(names(&c), [("edit", Status::Modified), ("fresh", Status::Added), ("gone", Status::Removed)]);
        assert_eq!(c[0].calls, ["helper"]);
        assert_eq!(c[1].calls, ["edit"]);
        assert_eq!(c[0].old, Some((5, 7)));
        assert_eq!(c[0].new, Some((6, 8)));
    }

    #[test]
    fn new_file_lists_everything_as_added() {
        let c = changed_functions("a.tsx", "", "export const A = () => <div />;\n");
        assert_eq!(names(&c), [("A", Status::Added)]);
    }

    #[test]
    fn jsx_children_count_as_calls() {
        let src = "export const Page = () => {\n  const x = useThing();\n  return <Layout.Body><Card x={x} /></Layout.Body>;\n};\n";
        let c = changed_functions("p.tsx", "", src);
        assert_eq!(c[0].calls, ["useThing", "Body", "Card"]);
    }
}
