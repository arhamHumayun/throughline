#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::panic)]
#![deny(clippy::todo)]
#![deny(clippy::unimplemented)]
#![deny(clippy::dbg_macro)]
#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]

pub mod analysis;
pub mod ast;
pub mod cluster;
pub mod config;
pub mod entrypoint;
pub mod flow;
pub mod flows;
pub mod git;
pub mod graph;
pub mod group_metadata;
pub mod ir;
pub mod naming;
pub mod output;
pub mod paths;
pub mod pipeline;
pub mod query_engine;
pub mod rank;
pub mod tsconfig;
pub mod types;
