//! Stack-ordered flows: where a change starts and what it calls.
//!
//! A flow's root is a changed source file that no other changed source file calls.
//! Its members are the changed files reachable from it, in breadth-first (reading)
//! order. A shared callee appears under every root that reaches it.
//!
//! Tests and stories are never roots. Tests call code, so treating them as entry
//! points puts them at the top of every flow; instead they're attached to the flows
//! whose files they exercise.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::Serialize;

use crate::graph::SymbolGraph;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Flow {
    /// Plain-language name (see `naming`); the root file's name until named.
    pub title: String,
    /// The commit the title came from, if any.
    pub commit: Option<String>,
    /// The entry file: changed, and called by no other changed source file.
    pub root: String,
    /// Changed source files reachable from `root`, root first, in BFS order.
    pub files: Vec<String>,
    /// Changed tests/stories that call into this flow's files.
    pub tests: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct Flows {
    /// Largest first.
    pub flows: Vec<Flow>,
    /// Changed source files with no edges to or from other changed files.
    pub standalone: Vec<String>,
    /// Changed tests and stories that touch no flow.
    pub orphan_tests: Vec<String>,
    /// Changed generated files (codegen output). Never part of a flow.
    pub generated: Vec<String>,
}

/// Codegen output: a path segment starting with `generated` or `__generated__`, or `.generated.` in the name.
pub fn is_generated(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.split('/').any(|seg| seg.starts_with("generated") || seg == "__generated__")
        || lower.rsplit('/').next().is_some_and(|n| n.contains(".generated."))
}

/// Tests, stories and snapshots: code that exercises the change rather than being it.
pub fn is_test_or_story(path: &str) -> bool {
    let lower = path.to_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    lower.contains("__tests__/")
        || lower.contains("__stories__/")
        || lower.contains("__snapshots__/")
        || lower.contains("/test/")
        || lower.contains("/tests/")
        || lower.starts_with("test/")
        || lower.starts_with("tests/")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.contains(".stories.")
        || name.contains(".integration-spec.")
        || name.ends_with(".snap")
        || (name.ends_with(".py") && (name.starts_with("test_") || name.ends_with("_test.py")))
        || name == "conftest.py"
}

/// Compute flows over `changed` files using file-level edges from `graph`.
pub fn compute(graph: &SymbolGraph, changed: &[String]) -> Flows {
    let changed_set: BTreeSet<&str> = changed.iter().map(String::as_str).collect();
    let file_of = |id: &str| -> Option<String> {
        let f = graph
            .get_symbol(id)
            .map(|s| s.file.clone())
            .unwrap_or_else(|| id.split("::").next().unwrap_or(id).to_string());
        changed_set.contains(f.as_str()).then_some(f)
    };

    // File-level adjacency among changed files (BTree for deterministic order).
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (from, to, _) in graph.edges() {
        if let (Some(a), Some(b)) = (file_of(from), file_of(to)) {
            if a != b {
                out.entry(a).or_default().insert(b);
            }
        }
    }

    let generated: Vec<String> =
        changed_set.iter().filter(|p| is_generated(p)).map(|p| p.to_string()).collect();
    let (tests, sources): (Vec<&str>, Vec<&str>) = changed_set
        .iter()
        .copied()
        .filter(|p| !is_generated(p))
        .partition(|p| is_test_or_story(p));
    let src_set: BTreeSet<&str> = sources.iter().copied().collect();

    let callees = |f: &str| -> Vec<&str> {
        out.get(f)
            .map(|s| s.iter().map(String::as_str).filter(|c| src_set.contains(c)).collect())
            .unwrap_or_default()
    };

    let mut has_caller: BTreeSet<&str> = BTreeSet::new();
    let mut connected: BTreeSet<&str> = BTreeSet::new();
    for &f in &sources {
        for c in callees(f) {
            has_caller.insert(c);
            connected.insert(c);
            connected.insert(f);
        }
    }

    let reach = |root: &str| -> Vec<String> {
        let mut seen = BTreeSet::from([root.to_string()]);
        let mut order = vec![root.to_string()];
        let mut q = VecDeque::from([root.to_string()]);
        while let Some(x) = q.pop_front() {
            for c in callees(&x) {
                if seen.insert(c.to_string()) {
                    order.push(c.to_string());
                    q.push_back(c.to_string());
                }
            }
        }
        order
    };

    let mut roots: Vec<&str> = sources
        .iter()
        .copied()
        .filter(|f| connected.contains(f) && !has_caller.contains(f))
        .collect();

    // Cycles with no outside caller have no root; seed one per uncovered cycle.
    let mut covered: BTreeSet<String> = roots.iter().flat_map(|r| reach(r)).collect();
    loop {
        let next = sources
            .iter()
            .copied()
            .filter(|f| connected.contains(f) && !covered.contains(*f))
            .max_by_key(|f| (callees(f).len(), std::cmp::Reverse(*f)));
        let Some(seed) = next else { break };
        roots.push(seed);
        covered.extend(reach(seed));
    }

    let mut flows: Vec<Flow> = roots
        .iter()
        .map(|r| Flow {
            title: crate::naming::root_title(r),
            commit: None,
            root: r.to_string(),
            files: reach(r),
            tests: Vec::new(),
        })
        .collect();

    let mut orphan_tests = Vec::new();
    for t in tests {
        let targets: BTreeSet<&str> = out
            .get(t)
            .map(|s| s.iter().map(String::as_str).collect())
            .unwrap_or_default();
        let mut attached = false;
        for flow in &mut flows {
            if flow.files.iter().any(|f| targets.contains(f.as_str())) {
                flow.tests.push(t.to_string());
                attached = true;
            }
        }
        if !attached {
            orphan_tests.push(t.to_string());
        }
    }

    flows.sort_by(|a, b| b.files.len().cmp(&a.files.len()).then_with(|| a.root.cmp(&b.root)));
    let standalone = sources
        .iter()
        .filter(|f| !connected.contains(*f))
        .map(|f| f.to_string())
        .collect();

    Flows { flows, standalone, orphan_tests, generated }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::graph::GraphEdge;
    use crate::types::EdgeType;

    fn graph(edges: &[(&str, &str)]) -> SymbolGraph {
        let files: Vec<crate::ast::ParsedFile> = edges
            .iter()
            .flat_map(|(a, b)| [*a, *b])
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|p| crate::ast::ParsedFile {
                path: p.to_string(),
                language: crate::ast::Language::TypeScript,
                definitions: vec![],
                imports: vec![],
                exports: vec![],
                call_sites: vec![],
            })
            .collect();
        let mut g = SymbolGraph::build(&files);
        for (a, b) in edges {
            let (x, y) = (g.get_node(a).unwrap(), g.get_node(b).unwrap());
            g.add_edge(x, y, GraphEdge { edge_type: EdgeType::Calls });
        }
        g
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn tests_are_not_roots_and_attach_to_flows() {
        let g = graph(&[
            ("src/api.ts", "src/svc.ts"),
            ("src/svc.ts", "src/db.ts"),
            ("src/__tests__/svc.test.ts", "src/svc.ts"),
        ]);
        let f = compute(&g, &s(&["src/api.ts", "src/svc.ts", "src/db.ts", "src/__tests__/svc.test.ts", "src/lone.ts"]));
        assert_eq!(f.flows.len(), 1);
        assert_eq!(f.flows[0].root, "src/api.ts");
        assert_eq!(f.flows[0].files, s(&["src/api.ts", "src/svc.ts", "src/db.ts"]));
        assert_eq!(f.flows[0].tests, s(&["src/__tests__/svc.test.ts"]));
        assert_eq!(f.standalone, s(&["src/lone.ts"]));
    }

    #[test]
    fn shared_callee_appears_under_each_root() {
        let g = graph(&[("a.ts", "util.ts"), ("b.ts", "util.ts")]);
        let f = compute(&g, &s(&["a.ts", "b.ts", "util.ts"]));
        assert_eq!(f.flows.len(), 2);
        assert!(f.flows.iter().all(|fl| fl.files.contains(&"util.ts".to_string())));
    }

    #[test]
    fn a_cycle_without_outside_callers_still_gets_a_root() {
        let g = graph(&[("x.ts", "y.ts"), ("y.ts", "x.ts")]);
        let f = compute(&g, &s(&["x.ts", "y.ts"]));
        assert_eq!(f.flows.len(), 1);
        assert_eq!(f.flows[0].files.len(), 2);
    }

    #[test]
    fn generated_files_stay_out_of_flows() {
        let g = graph(&[("src/a.tsx", "src/generated/graphql.ts")]);
        let f = compute(&g, &s(&["src/a.tsx", "src/generated/graphql.ts"]));
        assert!(f.flows.is_empty());
        assert_eq!(f.generated, s(&["src/generated/graphql.ts"]));
        assert!(is_generated("pkg/src/generated-metadata/schema.ts"));
        assert!(!is_generated("pkg/src/generator.ts"));
    }

    #[test]
    fn unchanged_files_do_not_count() {
        let g = graph(&[("a.ts", "unchanged.ts"), ("unchanged.ts", "b.ts")]);
        let f = compute(&g, &s(&["a.ts", "b.ts"]));
        assert!(f.flows.is_empty());
        assert_eq!(f.standalone, s(&["a.ts", "b.ts"]));
    }
}
