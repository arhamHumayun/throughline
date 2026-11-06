//! TypeScript path aliases (`compilerOptions.paths` / `baseUrl`) from tsconfig files.
//!
//! Real TS repos import through aliases (`@/foo`, `src/foo`, `~/foo`) far more
//! than through relative paths. Without resolving them, the symbol graph has
//! almost no edges between changed files.
//!
//! Scope: for each changed file, the nearest `tsconfig.json` walking up to the
//! repo root, following relative `extends`. Read from the working tree.

use std::collections::HashMap;
use std::path::Path;

/// Aliases from one tsconfig, applying to files under `dir`.
#[derive(Debug, Clone, Default)]
pub struct AliasScope {
    /// Repo-relative directory the tsconfig lives in ("" for the repo root).
    pub dir: String,
    /// Repo-relative `baseUrl`, if set.
    pub base_url: Option<String>,
    /// `paths` entries: pattern (may end in `*`) → repo-relative targets (may contain `*`).
    pub paths: Vec<(String, Vec<String>)>,
}

/// Every alias scope that applies to the analyzed files.
#[derive(Debug, Clone, Default)]
pub struct PathAliases {
    scopes: Vec<AliasScope>,
}

impl PathAliases {
    pub fn from_scopes(mut scopes: Vec<AliasScope>) -> Self {
        // Deepest directory first, so the nearest tsconfig wins.
        scopes.sort_by(|a, b| b.dir.len().cmp(&a.dir.len()));
        PathAliases { scopes }
    }

    pub fn is_empty(&self) -> bool {
        self.scopes.is_empty()
    }

    /// Load the tsconfigs governing `files` (repo-relative paths).
    pub fn load(repo_root: &Path, files: &[&str]) -> Self {
        let mut by_dir: HashMap<String, Option<AliasScope>> = HashMap::new();
        for file in files {
            let mut dir = parent(file);
            loop {
                if let Some(found) = by_dir.get(&dir) {
                    if found.is_some() {
                        break;
                    }
                } else {
                    let scope = read_scope(repo_root, &dir);
                    let found = scope.is_some();
                    by_dir.insert(dir.clone(), scope);
                    if found {
                        break;
                    }
                }
                if dir.is_empty() {
                    break;
                }
                dir = parent(&dir);
            }
        }
        Self::from_scopes(by_dir.into_values().flatten().collect())
    }

    /// Candidate repo-relative paths (no extension) an import may refer to.
    pub fn candidates(&self, import_source: &str, importer_path: &str) -> Vec<String> {
        if import_source.starts_with('.') {
            return Vec::new();
        }
        let Some(scope) = self.scopes.iter().find(|s| within(importer_path, &s.dir)) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (pattern, targets) in &scope.paths {
            let captured = match pattern.strip_suffix('*') {
                Some(prefix) => import_source.strip_prefix(prefix),
                None => (import_source == pattern).then_some(""),
            };
            if let Some(rest) = captured {
                for t in targets {
                    out.push(normalize(&t.replacen('*', rest, 1)));
                }
            }
        }
        if let Some(base) = &scope.base_url {
            out.push(normalize(&join(base, import_source)));
        }
        out
    }
}

fn within(path: &str, dir: &str) -> bool {
    dir.is_empty() || path.starts_with(&format!("{dir}/"))
}

fn parent(path: &str) -> String {
    path.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default()
}

fn join(dir: &str, rel: &str) -> String {
    if dir.is_empty() {
        rel.to_string()
    } else {
        format!("{dir}/{rel}")
    }
}

/// Collapse `.` and `..` segments.
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Read `<dir>/tsconfig.json` and return its alias scope, or None if it has no aliases.
fn read_scope(repo_root: &Path, dir: &str) -> Option<AliasScope> {
    let rel = join(dir, "tsconfig.json");
    let (base_url, paths) = read_options(repo_root, &rel, 0)?;
    if base_url.is_none() && paths.is_empty() {
        return None;
    }
    Some(AliasScope { dir: dir.to_string(), base_url, paths })
}

type Options = (Option<String>, Vec<(String, Vec<String>)>);

/// Resolve `baseUrl` and `paths` for one tsconfig file, following relative `extends`.
/// All returned paths are repo-relative.
fn read_options(repo_root: &Path, rel: &str, depth: u8) -> Option<Options> {
    if depth > 8 {
        return None;
    }
    let text = std::fs::read_to_string(repo_root.join(rel)).ok()?;
    let json: serde_json::Value = serde_json::from_str(&strip_jsonc(&text)).ok()?;
    let dir = parent(rel);

    let (mut base_url, mut paths) = match json.get("extends").and_then(|v| v.as_str()) {
        Some(ext) if ext.starts_with('.') => {
            let mut target = normalize(&join(&dir, ext));
            if !target.ends_with(".json") {
                target.push_str(".json");
            }
            read_options(repo_root, &target, depth + 1).unwrap_or_default()
        }
        _ => (None, Vec::new()),
    };

    let opts = json.get("compilerOptions");
    if let Some(b) = opts.and_then(|o| o.get("baseUrl")).and_then(|v| v.as_str()) {
        base_url = Some(normalize(&join(&dir, b)));
    }
    if let Some(p) = opts.and_then(|o| o.get("paths")).and_then(|v| v.as_object()) {
        // Targets in `paths` are relative to baseUrl if set, else to this tsconfig's dir.
        let anchor = base_url.clone().unwrap_or_else(|| dir.clone());
        paths = p
            .iter()
            .map(|(pattern, targets)| {
                let ts = targets
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|t| t.as_str())
                            .map(|t| normalize(&join(&anchor, t)))
                            .collect()
                    })
                    .unwrap_or_default();
                (pattern.clone(), ts)
            })
            .collect();
    }
    Some((base_url, paths))
}

/// Strip `//` and `/* */` comments and trailing commas, leaving strings intact.
fn strip_jsonc(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c as char);
            if c == b'\\' && i + 1 < b.len() {
                out.push(b[i + 1] as char);
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += 1;
        } else if c == b'"' {
            in_str = true;
            out.push('"');
            i += 1;
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else if c == b',' {
            let mut j = i + 1;
            while j < b.len() && (b[j] as char).is_whitespace() {
                j += 1;
            }
            if !matches!(b.get(j), Some(b'}') | Some(b']')) {
                out.push(',');
            }
            i += 1;
        } else {
            out.push(c as char);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn scope(dir: &str, base: Option<&str>, paths: &[(&str, &[&str])]) -> AliasScope {
        AliasScope {
            dir: dir.into(),
            base_url: base.map(Into::into),
            paths: paths
                .iter()
                .map(|(p, t)| (p.to_string(), t.iter().map(|s| s.to_string()).collect()))
                .collect(),
        }
    }

    #[test]
    fn wildcard_alias_resolves_under_its_package() {
        let a = PathAliases::from_scopes(vec![scope(
            "packages/front",
            None,
            &[("@/*", &["packages/front/src/modules/*"])],
        )]);
        assert_eq!(
            a.candidates("@/emails/hooks/useX", "packages/front/src/a.tsx"),
            vec!["packages/front/src/modules/emails/hooks/useX"]
        );
        assert!(a.candidates("@/x", "packages/server/src/a.ts").is_empty());
        assert!(a.candidates("./x", "packages/front/src/a.tsx").is_empty());
    }

    #[test]
    fn nearest_scope_wins_and_base_url_applies() {
        let a = PathAliases::from_scopes(vec![
            scope("", Some(""), &[]),
            scope("packages/server", Some("packages/server"), &[]),
        ]);
        assert_eq!(
            a.candidates("src/engine/x", "packages/server/src/a.ts"),
            vec!["packages/server/src/engine/x"]
        );
    }

    #[test]
    fn jsonc_comments_and_trailing_commas_are_stripped() {
        let s = strip_jsonc("{\n // c\n \"a\": \"//not\", /* x */ \"b\": [1,2,],\n}");
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["a"], "//not");
        assert_eq!(v["b"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn loads_extends_and_paths_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("app/src")).unwrap();
        std::fs::write(
            root.join("tsconfig.base.json"),
            r#"{ "compilerOptions": { "baseUrl": "." } }"#,
        )
        .unwrap();
        std::fs::write(
            root.join("app/tsconfig.json"),
            r#"{ "extends": "../tsconfig.base.json", "compilerOptions": { "paths": { "~/*": ["./app/src/*"] } } }"#,
        )
        .unwrap();
        let a = PathAliases::load(root, &["app/src/main.ts"]);
        assert_eq!(a.candidates("~/util", "app/src/main.ts")[0], "app/src/util");
    }
}
