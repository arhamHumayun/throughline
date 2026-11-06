#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr
)]
//! End-to-end integration tests for the full diffcore analysis pipeline.
//!
//! These tests create real git repositories with known file structures,
//! commit changes, and run the complete pipeline:
//!   git diff → AST parse → graph build → entrypoint detect →
//!   flow analyze → enrich graph → cluster → rank → output
//!
//! Run with:
//!   cargo test --test e2e_pipeline

mod helpers;

use git2::Repository;

use throughline_core::ast;
use throughline_core::entrypoint;
use throughline_core::git;
use throughline_core::output;
use throughline_core::types::AnalysisOutput;
use helpers::graph_assertions::{
    assert_all_files_accounted, assert_json_roundtrip, assert_language_detected,
    assert_valid_json_schema, assert_valid_mermaid, assert_valid_scores,
};
use helpers::repo_builder::{run_pipeline, RepoBuilder};

// ─── E2E Tests ───────────────────────────────────────────────────────────

/// Test: Simple 5-file Express app with a new route added.
///
/// Initial state: route.ts → service.ts → repo.ts (existing handler chain)
/// Change: add a new POST /users route + userService + userRepo
/// Expected: produces at least 1 flow group, files in flow order, valid JSON
#[test]
fn test_e2e_simple_express_app() {
    let rb = RepoBuilder::new();

    // Initial commit: base Express app
    rb.write_file(
        "src/routes/health.ts",
        r#"
import express from 'express';
const router = express.Router();

export function healthCheck(req: any, res: any) {
    res.json({ status: 'ok' });
}

router.get('/health', healthCheck);
export default router;
"#,
    );
    rb.write_file(
        "src/services/healthService.ts",
        r#"
export function getHealthStatus(): string {
    return 'ok';
}
"#,
    );
    rb.write_file(
        "package.json",
        r#"{"name": "test-app", "version": "1.0.0"}"#,
    );
    rb.commit("Initial commit: health endpoint");
    rb.create_branch("main");

    // Feature branch: add user creation flow
    rb.create_branch("feature/add-users");
    rb.checkout("feature/add-users");

    rb.write_file(
        "src/routes/users.ts",
        r#"
import express from 'express';
import { createUser } from '../services/userService';

const router = express.Router();

export function postUser(req: any, res: any) {
    const user = createUser(req.body);
    res.status(201).json(user);
}

router.post('/users', postUser);
export default router;
"#,
    );
    rb.write_file(
        "src/services/userService.ts",
        r#"
import { insertUser } from '../repositories/userRepo';

export function createUser(data: any) {
    const user = { id: Date.now(), ...data };
    return insertUser(user);
}
"#,
    );
    rb.write_file(
        "src/repositories/userRepo.ts",
        r#"
const users: any[] = [];

export function insertUser(user: any) {
    users.push(user);
    return user;
}

export function findUserById(id: number) {
    return users.find(u => u.id === id);
}
"#,
    );
    // Also modify health to add version info (simulates multi-area change)
    rb.write_file(
        "src/routes/health.ts",
        r#"
import express from 'express';
const router = express.Router();

export function healthCheck(req: any, res: any) {
    res.json({ status: 'ok', version: '2.0.0' });
}

router.get('/health', healthCheck);
export default router;
"#,
    );
    rb.commit("Add user creation flow + update health");

    // Run the full pipeline
    let output = run_pipeline(rb.path(), "main", "feature/add-users");

    // Assertions
    assert_eq!(output.version, "1.0.0");
    assert!(
        output.summary.total_files_changed >= 3,
        "should have at least 3 changed files"
    );
    assert_language_detected(&output, "typescript");
    assert_all_files_accounted(&output);
    assert_json_roundtrip(&output);
}

/// Test: Python FastAPI app with endpoint + service + repository.
///
/// Verifies entrypoint detection for Python decorators and DB write heuristics.
#[test]
fn test_e2e_python_fastapi() {
    let rb = RepoBuilder::new();

    rb.write_file("requirements.txt", "fastapi\nuvicorn\nsqlalchemy\n");
    rb.commit("Initial commit");
    rb.create_branch("main");

    rb.create_branch("feature/add-items");
    rb.checkout("feature/add-items");

    rb.write_file(
        "app/routes/items.py",
        r#"
from fastapi import APIRouter, Depends
from app.services.item_service import create_item
from app.schemas.item import ItemCreate

router = APIRouter()

@router.post("/items")
async def post_item(item: ItemCreate):
    return create_item(item)
"#,
    );
    rb.write_file(
        "app/services/item_service.py",
        r#"
from app.repositories.item_repo import save_item

def create_item(item_data):
    item = {"id": 1, "name": item_data.name}
    save_item(item)
    return item
"#,
    );
    rb.write_file(
        "app/repositories/item_repo.py",
        r#"
items_db = []

def save_item(item):
    items_db.append(item)
    return item

def find_item(item_id):
    return next((i for i in items_db if i["id"] == item_id), None)
"#,
    );
    rb.write_file(
        "app/schemas/item.py",
        r#"
class ItemCreate:
    name: str
"#,
    );
    rb.commit("Add items API");

    let output = run_pipeline(rb.path(), "main", "feature/add-items");

    assert!(output.summary.total_files_changed >= 4);
    assert_language_detected(&output, "python");
    assert_all_files_accounted(&output);
}

/// Test: Branch comparison produces correct diff source metadata.
#[test]
fn test_e2e_branch_comparison_metadata() {
    let rb = RepoBuilder::new();

    rb.write_file("src/index.ts", "export const VERSION = '1.0.0';\n");
    rb.commit("Initial");
    rb.create_branch("main");

    rb.create_branch("feature/bump");
    rb.checkout("feature/bump");
    rb.write_file("src/index.ts", "export const VERSION = '2.0.0';\n");
    rb.commit("Bump version");

    let output = run_pipeline(rb.path(), "main", "feature/bump");

    assert_eq!(
        output.diff_source.diff_type,
        throughline_core::types::DiffType::BranchComparison
    );
    assert_eq!(output.diff_source.base.as_deref(), Some("main"));
    assert_eq!(output.diff_source.head.as_deref(), Some("feature/bump"));
    assert!(output.diff_source.base_sha.is_some());
    assert!(output.diff_source.head_sha.is_some());
    assert_ne!(output.diff_source.base_sha, output.diff_source.head_sha);
}

/// Test: Empty diff (same ref) produces graceful empty result.
#[test]
fn test_e2e_no_changes() {
    let rb = RepoBuilder::new();

    rb.write_file("src/app.ts", "console.log('hello');\n");
    rb.commit("Initial");
    rb.create_branch("main");

    // Compare main to itself → no changes
    let output = run_pipeline(rb.path(), "main", "main");

    assert_eq!(output.summary.total_files_changed, 0);
    assert_eq!(output.summary.total_groups, 0);
    assert!(output.groups.is_empty());
    assert!(output.annotations.is_none());
}

/// Test: JSON output is valid and conforms to the documented schema.
#[test]
fn test_e2e_json_output_valid() {
    let rb = RepoBuilder::new();

    rb.write_file("src/handler.ts", "export function handle() {}\n");
    rb.commit("Initial");
    rb.create_branch("main");

    rb.create_branch("feature/x");
    rb.checkout("feature/x");
    rb.write_file(
        "src/handler.ts",
        r#"
import express from 'express';

export function handle(req: any, res: any) {
    res.json({ ok: true });
}

const app = express();
app.get('/test', handle);
"#,
    );
    rb.write_file(
        "src/utils.ts",
        "export function log(msg: string) { console.log(msg); }\n",
    );
    rb.commit("Add handler + utils");

    let output = run_pipeline(rb.path(), "main", "feature/x");

    assert_valid_json_schema(&output);
    assert_json_roundtrip(&output);
}

/// Test: Cross-cutting refactor — a shared utility used by many files.
///
/// All files should be grouped, and the shared file should not be lost.
#[test]
fn test_e2e_cross_cutting_refactor() {
    let rb = RepoBuilder::new();

    // Initial: 5 files all importing a shared utility
    rb.write_file(
        "src/utils/format.ts",
        "export function formatDate(d: Date): string { return d.toISOString(); }\n",
    );
    for i in 0..5 {
        rb.write_file(
            &format!("src/modules/mod{}.ts", i),
            &format!(
                "import {{ formatDate }} from '../utils/format';\nexport function handler{}() {{ return formatDate(new Date()); }}\n",
                i
            ),
        );
    }
    rb.commit("Initial with shared utility");
    rb.create_branch("main");

    rb.create_branch("feature/refactor");
    rb.checkout("feature/refactor");

    // Refactor: rename function and update all callers
    rb.write_file(
        "src/utils/format.ts",
        "export function formatDateTime(d: Date): string { return d.toLocaleString(); }\n",
    );
    for i in 0..5 {
        rb.write_file(
            &format!("src/modules/mod{}.ts", i),
            &format!(
                "import {{ formatDateTime }} from '../utils/format';\nexport function handler{}() {{ return formatDateTime(new Date()); }}\n",
                i
            ),
        );
    }
    rb.commit("Refactor: rename formatDate to formatDateTime");

    let output = run_pipeline(rb.path(), "main", "feature/refactor");

    // All 6 files should be changed
    assert_eq!(output.summary.total_files_changed, 6);
    assert_all_files_accounted(&output);
}

/// Test: Multiple entrypoints produce distinct flow groups.
///
/// Two independent handler chains: HTTP route and a queue consumer pattern.
#[test]
fn test_e2e_multiple_entrypoints() {
    let rb = RepoBuilder::new();

    rb.write_file("package.json", r#"{"name": "multi-ep"}"#);
    rb.commit("Initial");
    rb.create_branch("main");

    rb.create_branch("feature/multi");
    rb.checkout("feature/multi");

    // HTTP route chain
    rb.write_file(
        "src/routes/api.ts",
        r#"
import express from 'express';
import { processOrder } from '../services/orderService';

const router = express.Router();

export function createOrder(req: any, res: any) {
    const result = processOrder(req.body);
    res.json(result);
}

router.post('/orders', createOrder);
export default router;
"#,
    );
    rb.write_file(
        "src/services/orderService.ts",
        r#"
export function processOrder(data: any) {
    return { id: 1, ...data, status: 'created' };
}
"#,
    );

    // Queue consumer chain (independent)
    rb.write_file(
        "src/workers/emailWorker.ts",
        r#"
import { sendEmail } from '../services/emailService';

export function handleEmailJob(job: any) {
    sendEmail(job.to, job.subject, job.body);
}

process.on('message', handleEmailJob);
"#,
    );
    rb.write_file(
        "src/services/emailService.ts",
        r#"
export function sendEmail(to: string, subject: string, body: string) {
    console.log('Sending email to', to);
}
"#,
    );

    // Shared config
    rb.write_file(
        "src/config.ts",
        "export const DB_URL = process.env.DATABASE_URL;\n",
    );

    rb.commit("Add order API + email worker");

    let output = run_pipeline(rb.path(), "main", "feature/multi");

    assert!(output.summary.total_files_changed >= 5);
    assert_all_files_accounted(&output);
    assert_valid_scores(&output);
}

/// Test: Mixed TypeScript + Python project.
///
/// Validates multi-language support in a single analysis run.
#[test]
fn test_e2e_mixed_language() {
    let rb = RepoBuilder::new();

    rb.write_file("README.md", "# Mixed project\n");
    rb.commit("Initial");
    rb.create_branch("main");

    rb.create_branch("feature/mixed");
    rb.checkout("feature/mixed");

    rb.write_file(
        "frontend/src/api.ts",
        r#"
export async function fetchUsers() {
    const response = await fetch('/api/users');
    return response.json();
}
"#,
    );
    rb.write_file(
        "frontend/src/UserList.tsx",
        r#"
import { fetchUsers } from './api';

export function UserList() {
    const users = fetchUsers();
    return users;
}
"#,
    );
    rb.write_file(
        "backend/app/routes.py",
        r#"
from fastapi import APIRouter
from backend.app.services import get_all_users

router = APIRouter()

@router.get("/api/users")
def list_users():
    return get_all_users()
"#,
    );
    rb.write_file(
        "backend/app/services.py",
        r#"
def get_all_users():
    return [{"id": 1, "name": "Alice"}]
"#,
    );

    rb.commit("Add frontend + backend");

    let output = run_pipeline(rb.path(), "main", "feature/mixed");

    assert!(output.summary.total_files_changed >= 4);
    assert_language_detected(&output, "typescript");
    assert_language_detected(&output, "python");
}

/// Test: Deterministic output — running the same analysis twice produces identical results.
#[test]
fn test_e2e_deterministic() {
    let rb = RepoBuilder::new();

    rb.write_file("src/a.ts", "export function a() {}\n");
    rb.write_file(
        "src/b.ts",
        "import { a } from './a';\nexport function b() { return a(); }\n",
    );
    rb.commit("Initial");
    rb.create_branch("main");

    rb.create_branch("feature/det");
    rb.checkout("feature/det");
    rb.write_file("src/a.ts", "export function a() { return 42; }\n");
    rb.write_file(
        "src/b.ts",
        "import { a } from './a';\nexport function b() { return a() + 1; }\n",
    );
    rb.write_file(
        "src/c.ts",
        "import { b } from './b';\nexport function c() { return b() * 2; }\n",
    );
    rb.commit("Modify chain");

    let output1 = run_pipeline(rb.path(), "main", "feature/det");
    let output2 = run_pipeline(rb.path(), "main", "feature/det");

    let json1 = output::to_json(&output1).unwrap();
    let json2 = output::to_json(&output2).unwrap();

    assert_eq!(
        json1, json2,
        "two runs should produce identical JSON output"
    );
}

/// Test: New file additions (no old content) are handled correctly.
#[test]
fn test_e2e_new_files_only() {
    let rb = RepoBuilder::new();

    rb.write_file(".gitkeep", "");
    rb.commit("Initial empty");
    rb.create_branch("main");

    rb.create_branch("feature/new-files");
    rb.checkout("feature/new-files");

    rb.write_file(
        "src/index.ts",
        r#"
import { greet } from './greet';
console.log(greet('World'));
"#,
    );
    rb.write_file(
        "src/greet.ts",
        r#"
export function greet(name: string): string {
    return `Hello, ${name}!`;
}
"#,
    );
    rb.commit("Add initial files");

    let output = run_pipeline(rb.path(), "main", "feature/new-files");

    assert!(output.summary.total_files_changed >= 2);
    // Should not error even though there's no "old" version of these files
    let json = output::to_json(&output).unwrap();
    let _: serde_json::Value = serde_json::from_str(&json).unwrap();
}

/// Test: File with auth/security changes scores higher risk.
#[test]
fn test_e2e_risk_scoring_auth() {
    let rb = RepoBuilder::new();

    rb.write_file("src/auth/middleware.ts", "export function auth() {}\n");
    rb.write_file("src/utils/helper.ts", "export function help() {}\n");
    rb.commit("Initial");
    rb.create_branch("main");

    rb.create_branch("feature/auth-fix");
    rb.checkout("feature/auth-fix");
    rb.write_file(
        "src/auth/middleware.ts",
        "export function auth() { /* fixed vulnerability */ return true; }\n",
    );
    rb.write_file(
        "src/utils/helper.ts",
        "export function help() { return 'updated'; }\n",
    );
    rb.commit("Fix auth + update helper");

    let output = run_pipeline(rb.path(), "main", "feature/auth-fix");

    // Find the group containing the auth file
    let auth_group = output
        .groups
        .iter()
        .find(|g| g.files.iter().any(|f| f.path.contains("auth")));
    let helper_group = output
        .groups
        .iter()
        .find(|g| g.files.iter().any(|f| f.path.contains("helper")));

    // If they're in separate groups, auth group should have higher risk
    if let (Some(ag), Some(hg)) = (auth_group, helper_group) {
        if ag.id != hg.id {
            assert!(
                ag.risk_score >= hg.risk_score,
                "auth group risk ({}) should be >= helper group risk ({})",
                ag.risk_score,
                hg.risk_score
            );
        }
    }
}

/// Test: Large-ish diff (20 files) completes without error and produces reasonable output.
#[test]
fn test_e2e_20_file_diff() {
    let rb = RepoBuilder::new();

    rb.write_file("package.json", r#"{"name": "big-app"}"#);
    rb.commit("Initial");
    rb.create_branch("main");

    rb.create_branch("feature/big");
    rb.checkout("feature/big");

    // Create 20 TypeScript files with import chains
    for i in 0..20 {
        let content = if i == 0 {
            format!(
                "import express from 'express';\nconst router = express.Router();\nexport function handler{}(req: any, res: any) {{ res.json({{}}); }}\nrouter.get('/route{}', handler{});\nexport default router;\n",
                i, i, i
            )
        } else {
            format!(
                "import {{ handler{} }} from './file{}';\nexport function handler{}() {{ return handler{}(); }}\n",
                i - 1, i - 1, i, i - 1
            )
        };
        rb.write_file(&format!("src/file{}.ts", i), &content);
    }
    rb.commit("Add 20 files");

    let start = std::time::Instant::now();
    let output = run_pipeline(rb.path(), "main", "feature/big");
    let elapsed = start.elapsed();

    assert_eq!(output.summary.total_files_changed, 20);
    assert!(
        elapsed.as_secs() < 30,
        "20-file analysis should complete in <30s, took {:?}",
        elapsed
    );
    assert_all_files_accounted(&output);

    // Should produce valid JSON
    let json = output::to_json(&output).unwrap();
    let _: serde_json::Value = serde_json::from_str(&json).unwrap();
}

/// Test: Mermaid diagram generation for flow groups.
#[test]
fn test_e2e_mermaid_generation() {
    let rb = RepoBuilder::new();

    rb.write_file("base.ts", "export const x = 1;\n");
    rb.commit("Initial");
    rb.create_branch("main");

    rb.create_branch("feature/flow");
    rb.checkout("feature/flow");

    rb.write_file(
        "src/route.ts",
        r#"
import express from 'express';
import { doWork } from './service';

const router = express.Router();
export function handle(req: any, res: any) { res.json(doWork()); }
router.get('/work', handle);
"#,
    );
    rb.write_file(
        "src/service.ts",
        r#"
export function doWork() { return { result: 'done' }; }
"#,
    );
    rb.commit("Add route + service");

    let output = run_pipeline(rb.path(), "main", "feature/flow");

    assert_valid_mermaid(&output);
}

/// Test: Commit range support via diff_refs with commit SHAs.
#[test]
fn test_e2e_commit_range() {
    let rb = RepoBuilder::new();

    rb.write_file("src/app.ts", "export const v = 1;\n");
    let _c1 = rb.commit("Commit 1");

    rb.write_file("src/app.ts", "export const v = 2;\n");
    let _c2 = rb.commit("Commit 2");

    rb.write_file("src/app.ts", "export const v = 3;\n");
    rb.write_file("src/extra.ts", "export function extra() {}\n");
    let c3 = rb.commit("Commit 3");

    // Compare HEAD~2..HEAD (should include commits 2 and 3)
    let repo = Repository::open(rb.path()).unwrap();
    let head = repo.find_commit(c3).unwrap();
    let base = head.parent(0).unwrap().parent(0).unwrap();

    let diff_result =
        git::diff_refs(&repo, &base.id().to_string(), &head.id().to_string()).unwrap();

    assert!(
        diff_result.files.len() >= 1,
        "should have at least 1 changed file in range"
    );
}

/// Test: Entrypoint detection works for Express routes in e2e context.
#[test]
fn test_e2e_entrypoint_detection() {
    let rb = RepoBuilder::new();

    rb.write_file("init.ts", "// init\n");
    rb.commit("Initial");
    rb.create_branch("main");

    rb.create_branch("feature/ep");
    rb.checkout("feature/ep");

    rb.write_file(
        "src/server.ts",
        r#"
import express from 'express';

const app = express();

app.get('/api/health', (req, res) => { res.json({ ok: true }); });
app.post('/api/users', (req, res) => { res.json(req.body); });

export default app;
"#,
    );
    rb.commit("Add express routes");

    // Run just the AST + entrypoint part of the pipeline
    let repo = Repository::open(rb.path()).unwrap();
    let diff_result = git::diff_refs(&repo, "main", "feature/ep").unwrap();

    let mut parsed_files = Vec::new();
    for file_diff in &diff_result.files {
        if let Some(ref content) = file_diff.new_content {
            if let Ok(parsed) = ast::parse_file(file_diff.path(), content) {
                parsed_files.push(parsed);
            }
        }
    }

    let entrypoints = entrypoint::detect_entrypoints(&parsed_files);

    // Should detect HTTP route entrypoints
    let http_eps: Vec<_> = entrypoints
        .iter()
        .filter(|e| e.entrypoint_type == throughline_core::types::EntrypointType::HttpRoute)
        .collect();
    assert!(
        !http_eps.is_empty(),
        "should detect at least one HTTP route entrypoint, got: {:?}",
        entrypoints
    );
}

// ─── Go Integration Tests ────────────────────────────────────────────────

/// Test: Synthetic Go HTTP API with handler → service → repo pattern.
///
/// Creates a Go app with Gin framework:
///   main.go → handlers/user.go → services/user.go → repositories/user.go
///
/// Verifies:
///   - Go language detection
///   - Import extraction from Go files
///   - Function/struct/interface definitions
///   - Call site detection
///   - HTTP route entrypoint detection
///   - Framework detection (Gin)
///   - Pipeline produces valid groups and JSON output

/// Test: Go test file detection.
///
/// Verifies that `_test.go` files are detected as test file entrypoints,
/// and that Go Test* functions are recognized as test symbols.
#[test]
fn test_e2e_go_test_file_detection() {
    let rb = RepoBuilder::new();

    rb.write_file("go.mod", "module example.com/app\n\ngo 1.21\n");
    rb.commit("Initial commit");
    rb.create_branch("main");

    rb.create_branch("feature/tests");
    rb.checkout("feature/tests");

    rb.write_file(
        "handlers/user.go",
        r#"
package handlers

func GetUser(id string) string {
    return "user-" + id
}
"#,
    );

    rb.write_file(
        "handlers/user_test.go",
        r#"
package handlers

import "testing"

func TestGetUser(t *testing.T) {
    result := GetUser("123")
    if result != "user-123" {
        t.Errorf("unexpected: %s", result)
    }
}

func BenchmarkGetUser(b *testing.B) {
    for i := 0; i < b.N; i++ {
        GetUser("123")
    }
}
"#,
    );

    rb.commit("Add user handler with tests");

    let result = run_pipeline(rb.path(), "main", "feature/tests");

    // Verify test file entrypoint detection
    let test_eps: Vec<_> = result
        .groups
        .iter()
        .flat_map(|g| g.entrypoint.as_ref())
        .filter(|ep| ep.entrypoint_type == throughline_core::types::EntrypointType::TestFile)
        .collect();
    assert!(
        !test_eps.is_empty(),
        "should detect _test.go as test file entrypoint"
    );
}

// =====================================================================
// Rust language integration tests
// =====================================================================

/// Test: Synthetic Rust axum API with handler→service→repo pattern.
///
/// Creates a 5-file Rust HTTP API using axum and verifies the full pipeline:
/// language detection, import extraction, definition extraction, call sites,
/// entrypoint detection, framework detection, grouping, and JSON output.

/// Test: Rust test file detection.
///
/// Verifies that `_test.rs` files and functions with test_ prefix are detected.
#[test]
fn test_e2e_rust_test_file_detection() {
    let rb = RepoBuilder::new();

    rb.write_file(
        "Cargo.toml",
        r#"[package]
name = "my-app"
version = "0.1.0"
edition = "2021"
"#,
    );
    rb.commit("Initial commit");
    rb.create_branch("main");

    rb.create_branch("feature/tests");
    rb.checkout("feature/tests");

    rb.write_file(
        "src/lib.rs",
        r#"
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}
"#,
    );

    rb.write_file(
        "src/lib_test.rs",
        r#"
use crate::add;

fn test_add() {
    assert_eq!(add(2, 3), 5);
}

fn test_add_negative() {
    assert_eq!(add(-1, 1), 0);
}
"#,
    );

    rb.commit("Add lib with tests");

    let result = run_pipeline(rb.path(), "main", "feature/tests");

    // Verify test file detection
    let test_eps: Vec<_> = result
        .groups
        .iter()
        .flat_map(|g| g.entrypoint.as_ref())
        .filter(|ep| ep.entrypoint_type == throughline_core::types::EntrypointType::TestFile)
        .collect();
    assert!(
        !test_eps.is_empty(),
        "should detect _test.rs as test file entrypoint"
    );
}

// ---------------------------------------------------------------------------
// Java integration tests (Phase 11.2)
// ---------------------------------------------------------------------------

/// Test: Java Spring Boot REST API with controller → service → repository pattern.
///
/// Verifies full pipeline: language detection, file accounting, flow groups,
/// entrypoint detection, framework detection, and Mermaid graph.

/// Test: Java test file detection.
///
/// Verifies that *Test.java files and @Test annotated methods are detected as test entrypoints.
#[test]
fn test_e2e_java_test_file_detection() {
    let rb = RepoBuilder::new();

    rb.write_file(
        "pom.xml",
        r#"<project>
    <modelVersion>4.0.0</modelVersion>
    <groupId>com.example</groupId>
    <artifactId>demo</artifactId>
    <version>0.0.1-SNAPSHOT</version>
</project>
"#,
    );
    rb.commit("Initial commit");
    rb.create_branch("main");

    rb.create_branch("feature/tests");
    rb.checkout("feature/tests");

    rb.write_file(
        "src/main/java/com/example/demo/UserService.java",
        r#"
package com.example.demo;

public class UserService {
    public String greet(String name) {
        return "Hello, " + name;
    }
}
"#,
    );

    rb.write_file(
        "src/test/java/com/example/demo/UserServiceTest.java",
        r#"
package com.example.demo;

import org.junit.jupiter.api.Test;
import static org.junit.jupiter.api.Assertions.assertEquals;

public class UserServiceTest {

    @Test
    public void testGreet() {
        UserService svc = new UserService();
        assertEquals("Hello, Alice", svc.greet("Alice"));
    }

    @Test
    public void testGreetEmpty() {
        UserService svc = new UserService();
        assertEquals("Hello, ", svc.greet(""));
    }
}
"#,
    );

    rb.commit("Add UserService and tests");

    let result = run_pipeline(rb.path(), "main", "feature/tests");

    // Verify test file detection
    let test_eps: Vec<_> = result
        .groups
        .iter()
        .flat_map(|g| g.entrypoint.as_ref())
        .filter(|ep| ep.entrypoint_type == throughline_core::types::EntrypointType::TestFile)
        .collect();
    assert!(
        !test_eps.is_empty(),
        "should detect *Test.java as test file entrypoint"
    );
}

// ---------------------------------------------------------------------------
// C# integration tests (Phase 11.2)
// ---------------------------------------------------------------------------

/// Test: C# ASP.NET Core Web API with controller → service → repository pattern.
///
/// Verifies full pipeline: language detection, file accounting, flow groups,
/// entrypoint detection, framework detection, and Mermaid graph.

/// Test: C# test file detection.
///
/// Verifies that *Test.cs and *Tests.cs files are detected as test entrypoints.
#[test]
fn test_e2e_csharp_test_file_detection() {
    let rb = RepoBuilder::new();

    rb.write_file(
        "MyApp.csproj",
        r#"<Project Sdk="Microsoft.NET.Sdk.Web">
    <PropertyGroup>
        <TargetFramework>net8.0</TargetFramework>
    </PropertyGroup>
</Project>
"#,
    );
    rb.commit("Initial commit");
    rb.create_branch("main");

    rb.create_branch("feature/tests");
    rb.checkout("feature/tests");

    rb.write_file(
        "Services/UserService.cs",
        r#"
namespace MyApp.Services
{
    public class UserService
    {
        public string GetGreeting(string name)
        {
            return $"Hello, {name}!";
        }
    }
}
"#,
    );

    rb.write_file(
        "Tests/UserServiceTests.cs",
        r#"
using Xunit;
using MyApp.Services;

namespace MyApp.Tests
{
    public class UserServiceTests
    {
        [Fact]
        public void GetGreeting_ReturnsExpected()
        {
            var svc = new UserService();
            var result = svc.GetGreeting("World");
            Assert.Equal("Hello, World!", result);
        }
    }
}
"#,
    );

    rb.commit("Add UserService and tests");

    let result = run_pipeline(rb.path(), "main", "feature/tests");

    // Verify test file detection
    let test_eps: Vec<_> = result
        .groups
        .iter()
        .flat_map(|g| g.entrypoint.as_ref())
        .filter(|ep| ep.entrypoint_type == throughline_core::types::EntrypointType::TestFile)
        .collect();
    assert!(
        !test_eps.is_empty(),
        "should detect *Tests.cs as test file entrypoint"
    );
}

// ─── PHP E2E Tests ────────────────────────────────────────────────────────

/// Test: Synthetic Laravel REST API with controller → service → model pattern.
///
/// Verifies PHP parsing, import resolution, entrypoint detection (Laravel controllers),
/// framework detection (Laravel), and flow grouping.

/// Test: PHP test file detection.
///
/// Verifies that *Test.php files are detected as test entrypoints.

/// Test: Ruby Rails REST API with controller→service→model pattern.
///
/// Verifies that the pipeline can:
/// - Parse Ruby source files via tree-sitter
/// - Extract require/require_relative imports, include/extend mixins
/// - Detect class, module, and method definitions
/// - Detect Rails controller action entrypoints
/// - Detect Rails framework from imports
/// - Cluster files into meaningful flow groups

/// Test: Ruby test file detection.
///
/// Verifies that *_spec.rb and *_test.rb files are detected as test entrypoints.

/// Test: Kotlin Ktor REST API with handler→service→repo pattern.
///
/// Verifies that the pipeline can:
/// - Parse Kotlin source files via tree-sitter
/// - Extract import statements (regular, aliased, wildcard)
/// - Detect fun, class, object, val/var definitions
/// - Detect Ktor route handler entrypoints
/// - Detect Ktor framework from imports
/// - Cluster files into meaningful flow groups

/// Test: Kotlin test file detection.
///
/// Verifies that *Test.kt files are detected as test entrypoints.

/// Test: Swift Vapor REST API with controller→service→repo pattern.
///
/// Creates a synthetic Swift Vapor app to verify:
/// - Swift file detection (.swift extension)
/// - Import extraction (module-level imports)
/// - Definition extraction (struct, class, protocol, func)
/// - Call site extraction (method calls, function calls)
/// - Entrypoint detection (Vapor route handlers)
/// - Framework detection (Vapor, Fluent)
/// - Semantic grouping and ranking

/// Test: Swift test file detection.
///
/// Verifies that *Tests.swift and *Test.swift files are detected as test entrypoints.

// ─── C++ Integration Tests ──────────────────────────────────────────────

/// Test: Synthetic C++ REST API with handler→service→repo pattern.
///
/// Verifies: C++ language detection, #include import resolution,
/// class/function extraction, call graph across files, framework detection,
/// entrypoint detection (main), semantic grouping.

/// Test: C test file detection.
///
/// Verifies that *_test.c and files in test/ directories are detected as test entrypoints.

// ─── Scala Integration Tests ─────────────────────────────────────────────

/// Test: Synthetic Scala Akka HTTP API with handler → service → repository pattern.

/// Test: Scala test file detection with ScalaTest.

// ─── Spec §13.5 Missing Integration Tests ─────────────────────────────

/// Test: Next.js app — modify a page + API route + Prisma model.
///
/// Creates a Next.js fullstack app with API routes and React pages.
/// Expected: produces 2 groups (API flow and UI flow), correctly separated.
#[test]
fn test_e2e_nextjs_page_change() {
    let rb = RepoBuilder::new();

    // Initial commit: base Next.js app with existing page + API route
    rb.write_file(
        "package.json",
        r#"{"name": "nextjs-app", "dependencies": {"next": "14.0.0", "@prisma/client": "5.0.0"}}"#,
    );
    rb.write_file(
        "src/app/api/users/route.ts",
        r#"
import { NextResponse } from 'next/server';

export async function GET() {
    return NextResponse.json([]);
}
"#,
    );
    rb.write_file(
        "src/app/users/page.tsx",
        r#"
export default function UsersPage() {
    return <div>Users</div>;
}
"#,
    );
    rb.write_file(
        "prisma/schema.prisma",
        r#"
model User {
  id    String @id
  name  String
}
"#,
    );
    rb.commit("Initial Next.js app");
    rb.create_branch("main");

    rb.create_branch("feature/nextjs-products");
    rb.checkout("feature/nextjs-products");

    // Add new API route (products)
    rb.write_file(
        "src/app/api/products/route.ts",
        r#"
import { NextResponse } from 'next/server';
import { getProducts, createProduct } from '../../../services/productService';

export async function GET() {
    const products = await getProducts();
    return NextResponse.json(products);
}

export async function POST(request: Request) {
    const body = await request.json();
    const product = await createProduct(body);
    return NextResponse.json(product, { status: 201 });
}
"#,
    );

    // Add service layer
    rb.write_file(
        "src/services/productService.ts",
        r#"
import { PrismaClient } from '@prisma/client';

const prisma = new PrismaClient();

export async function getProducts() {
    return prisma.product.findMany();
}

export async function createProduct(data: any) {
    return prisma.product.create({ data });
}
"#,
    );

    // Add new page (products listing)
    rb.write_file(
        "src/app/products/page.tsx",
        r#"
import { ProductList } from '../../components/ProductList';

export default async function ProductsPage() {
    const res = await fetch('/api/products');
    const products = await res.json();
    return <ProductList products={products} />;
}
"#,
    );

    // Add component
    rb.write_file(
        "src/components/ProductList.tsx",
        r#"
export function ProductList({ products }: { products: any[] }) {
    return (
        <ul>
            {products.map(p => <li key={p.id}>{p.name}</li>)}
        </ul>
    );
}
"#,
    );

    // Update Prisma schema (add Product model)
    rb.write_file(
        "prisma/schema.prisma",
        r#"
model User {
  id    String @id
  name  String
}

model Product {
  id    String @id
  name  String
  price Float
}
"#,
    );

    rb.commit("Add product listing: API route + page + Prisma model");

    let output = run_pipeline(rb.path(), "main", "feature/nextjs-products");

    // Should have at least 4 changed files (API route, service, page, component, schema)
    assert!(
        output.summary.total_files_changed >= 4,
        "expected >= 4 changed files, got {}",
        output.summary.total_files_changed
    );

    // Should produce at least 2 groups (API flow and UI flow)
    assert!(
        output.summary.total_groups >= 2,
        "expected >= 2 flow groups for API + UI flows, got {}",
        output.summary.total_groups
    );

    assert_language_detected(&output, "typescript");
    assert_all_files_accounted(&output);
    assert_json_roundtrip(&output);
}

/// Test: 50-file diff completes within reasonable time and produces valid output.
///
/// Generates 50 TypeScript files with import chains and verifies
/// the pipeline handles them without timeout or OOM.
#[test]
fn test_e2e_50_file_diff() {
    let rb = RepoBuilder::new();

    rb.write_file("package.json", r#"{"name": "large-app-50"}"#);
    rb.commit("Initial");
    rb.create_branch("main");

    rb.create_branch("feature/50-files");
    rb.checkout("feature/50-files");

    // Create 50 files: 5 route entrypoints, each with a chain of 9 downstream files
    for group in 0..5 {
        let route_file = format!("src/routes/route{}.ts", group);
        rb.write_file(
            &route_file,
            &format!(
                "import express from 'express';\nimport {{ service{g} }} from '../services/svc{g}';\n\nconst router = express.Router();\nexport function handle{g}(req: any, res: any) {{ res.json(service{g}()); }}\nrouter.get('/route{g}', handle{g});\nexport default router;\n",
                g = group
            ),
        );
        for depth in 0..9 {
            let file = format!("src/services/svc{}_{}.ts", group, depth);
            let content = if depth == 0 {
                format!(
                    "import {{ fn{}_{} }} from './svc{}_{}';\nexport function service{}() {{ return fn{}_{}(); }}\n",
                    group, depth + 1, group, depth + 1, group, group, depth + 1
                )
            } else if depth < 8 {
                format!(
                    "import {{ fn{}_{} }} from './svc{}_{}';\nexport function fn{}_{}() {{ return fn{}_{}(); }}\n",
                    group, depth + 1, group, depth + 1, group, depth, group, depth + 1
                )
            } else {
                format!(
                    "export function fn{}_{}() {{ return {{ value: {} }}; }}\n",
                    group,
                    depth,
                    group * 10 + depth
                )
            };
            rb.write_file(&file, &content);
        }
    }
    rb.commit("Add 50 files across 5 route groups");

    let start = std::time::Instant::now();
    let output = run_pipeline(rb.path(), "main", "feature/50-files");
    let elapsed = start.elapsed();

    assert_eq!(
        output.summary.total_files_changed, 50,
        "expected 50 changed files"
    );
    assert!(
        elapsed.as_secs() < 5,
        "50-file analysis should complete in <5s, took {:?}",
        elapsed
    );
    assert_all_files_accounted(&output);
    assert_valid_scores(&output);

    let json = output::to_json(&output).unwrap();
    let _: serde_json::Value = serde_json::from_str(&json).unwrap();
}

/// Test: 100-file diff completes within reasonable time without OOM.
///
/// Generates 100 TypeScript files with import chains. Verifies the
/// pipeline scales to large diffs without panicking or timing out.
#[test]
fn test_e2e_100_file_diff() {
    let rb = RepoBuilder::new();

    rb.write_file("package.json", r#"{"name": "large-app-100"}"#);
    rb.commit("Initial");
    rb.create_branch("main");

    rb.create_branch("feature/100-files");
    rb.checkout("feature/100-files");

    // Create 100 files: 10 route entrypoints, each with a chain of 9 downstream files
    for group in 0..10 {
        let route_file = format!("src/routes/route{}.ts", group);
        rb.write_file(
            &route_file,
            &format!(
                "import express from 'express';\nimport {{ svc{g} }} from '../services/svc{g}';\n\nconst router = express.Router();\nexport function handler{g}(req: any, res: any) {{ res.json(svc{g}()); }}\nrouter.get('/r{g}', handler{g});\nexport default router;\n",
                g = group
            ),
        );
        for depth in 0..9 {
            let file = format!("src/services/svc{}_{}.ts", group, depth);
            let content = if depth == 0 {
                format!(
                    "import {{ f{}_{} }} from './svc{}_{}';\nexport function svc{}() {{ return f{}_{}(); }}\n",
                    group, depth + 1, group, depth + 1, group, group, depth + 1
                )
            } else if depth < 8 {
                format!(
                    "import {{ f{}_{} }} from './svc{}_{}';\nexport function f{}_{}() {{ return f{}_{}(); }}\n",
                    group, depth + 1, group, depth + 1, group, depth, group, depth + 1
                )
            } else {
                format!(
                    "export function f{}_{}() {{ return {{ v: {} }}; }}\n",
                    group,
                    depth,
                    group * 10 + depth
                )
            };
            rb.write_file(&file, &content);
        }
    }
    rb.commit("Add 100 files across 10 route groups");

    let start = std::time::Instant::now();
    let output = run_pipeline(rb.path(), "main", "feature/100-files");
    let elapsed = start.elapsed();

    assert_eq!(
        output.summary.total_files_changed, 100,
        "expected 100 changed files"
    );
    assert!(
        elapsed.as_secs() < 15,
        "100-file analysis should complete in <15s, took {:?}",
        elapsed
    );
    assert_all_files_accounted(&output);
    assert_valid_scores(&output);

    // Should produce valid JSON
    let json = output::to_json(&output).unwrap();
    let _: serde_json::Value = serde_json::from_str(&json).unwrap();
}

/// Test: Staged changes — only staged files are included in the diff.
///
/// Stages some files but leaves others unstaged, then runs diff_staged
/// and verifies only the staged files appear in the result.
#[test]
fn test_e2e_staged_changes() {
    let rb = RepoBuilder::new();

    // Create initial files
    rb.write_file(
        "src/handler.ts",
        "export function handle() { return 'v1'; }\n",
    );
    rb.write_file(
        "src/service.ts",
        "export function serve() { return 'v1'; }\n",
    );
    rb.write_file("src/utils.ts", "export function util() { return 'v1'; }\n");
    rb.commit("Initial commit");

    // Modify all three files in the working directory
    rb.write_file(
        "src/handler.ts",
        "export function handle() { return 'v2-staged'; }\n",
    );
    rb.write_file(
        "src/service.ts",
        "export function serve() { return 'v2-staged'; }\n",
    );
    rb.write_file(
        "src/utils.ts",
        "export function util() { return 'v2-unstaged'; }\n",
    );

    // Stage only handler.ts and service.ts (NOT utils.ts)
    {
        let repo = rb.repo();
        let mut index = repo.index().unwrap();
        index
            .add_path(std::path::Path::new("src/handler.ts"))
            .unwrap();
        index
            .add_path(std::path::Path::new("src/service.ts"))
            .unwrap();
        index.write().unwrap();
    }

    // Run diff_staged
    let repo = git2::Repository::open(rb.path()).unwrap();
    let diff_result = git::diff_staged(&repo).unwrap();

    // Should include exactly the 2 staged files
    let staged_paths: Vec<&str> = diff_result.files.iter().map(|f| f.path()).collect();
    assert_eq!(
        staged_paths.len(),
        2,
        "expected 2 staged files, got {:?}",
        staged_paths
    );
    assert!(
        staged_paths.contains(&"src/handler.ts"),
        "handler.ts should be staged"
    );
    assert!(
        staged_paths.contains(&"src/service.ts"),
        "service.ts should be staged"
    );
    assert!(
        !staged_paths.contains(&"src/utils.ts"),
        "utils.ts should NOT be in staged diff"
    );

    // Verify the staged content is correct
    for file_diff in &diff_result.files {
        if let Some(ref new_content) = file_diff.new_content {
            assert!(
                new_content.contains("v2-staged"),
                "staged file {} should have v2-staged content",
                file_diff.path()
            );
        }
    }
}

/// Test: Config overrides — custom entrypoint globs detect files that heuristics miss.
///
/// Creates files in non-standard locations that aren't detected by built-in
/// heuristics, then provides a `.diffcore.toml` with custom entrypoint globs
/// that should pick them up.
#[test]
fn test_e2e_config_overrides() {
    let rb = RepoBuilder::new();

    rb.write_file("package.json", r#"{"name": "custom-app"}"#);
    rb.commit("Initial");
    rb.create_branch("main");

    rb.create_branch("feature/custom-ep");
    rb.checkout("feature/custom-ep");

    // Files in non-standard locations (no framework imports, no standard paths)
    // These would NOT be detected as entrypoints by default heuristics
    rb.write_file(
        "src/triggers/onUserCreated.ts",
        r#"
import { notifyAdmin } from '../notifications/admin';

export function handleUserCreated(event: any) {
    notifyAdmin(event.userId);
    return { processed: true };
}
"#,
    );
    rb.write_file(
        "src/triggers/onOrderPlaced.ts",
        r#"
import { processPayment } from '../billing/payment';

export function handleOrderPlaced(event: any) {
    processPayment(event.orderId, event.amount);
    return { processed: true };
}
"#,
    );
    rb.write_file(
        "src/notifications/admin.ts",
        r#"
export function notifyAdmin(userId: string) {
    console.log('Notifying admin about user:', userId);
}
"#,
    );
    rb.write_file(
        "src/billing/payment.ts",
        r#"
export function processPayment(orderId: string, amount: number) {
    console.log('Processing payment:', orderId, amount);
}
"#,
    );

    // Provide a config that declares triggers as event entrypoints
    rb.write_file(
        ".diffcore.toml",
        r#"
[entrypoints]
events = ["src/triggers/**/*.ts"]
"#,
    );

    rb.commit("Add triggers with custom config");

    // Run pipeline WITH config loaded from repo
    let config = throughline_core::config::DiffcoreConfig::load_from_dir(rb.path()).unwrap();

    // Verify config was loaded and has our custom entrypoints
    assert!(
        !config.entrypoints.events.is_empty(),
        "config should have event entrypoint globs"
    );

    // Resolve entrypoint globs
    let resolved = config.resolve_entrypoint_globs(rb.path());
    assert!(
        resolved
            .iter()
            .any(|p| p.to_string_lossy().contains("triggers")),
        "config should resolve trigger files; resolved: {:?}",
        resolved
    );

    // Run full pipeline and verify files are accounted for
    let output = run_pipeline(rb.path(), "main", "feature/custom-ep");

    assert!(
        output.summary.total_files_changed >= 4,
        "expected >= 4 changed files, got {}",
        output.summary.total_files_changed
    );
    assert_all_files_accounted(&output);
    assert_valid_json_schema(&output);
}
