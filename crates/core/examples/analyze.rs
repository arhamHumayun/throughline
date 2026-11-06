//! Dev harness, not a user-facing CLI: `cargo run --example analyze -- <repo> <base> <head>`
#![allow(clippy::print_stdout)]
use throughline_core::analysis::{analyze, DiffSpec};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 4 {
        return Err("usage: analyze <repo> <base> <head>".into());
    }
    let out = analyze(
        std::path::Path::new(&a[1]),
        &DiffSpec::Refs { base: a[2].clone(), head: a[3].clone() },
    )?;
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
