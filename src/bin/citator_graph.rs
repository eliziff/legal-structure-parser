//! Builds Beaver's citator graph from the local A2AJ and journal stores (see `legal_structure::citator_graph`).
//!
//!   citator-graph [--store a2aj.sqlite] [--journals public_endpoint.db] [--out noteup.sqlite, beside the store] [--courts SCC,ONCA] [--limit N] [--threads N]
use legal_structure::citator_graph::{build_citator_graph, default_journals, default_store, graph_path, GraphOptions};
use std::path::PathBuf;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let value = |name: &str| arguments.iter().position(|argument| argument == name).and_then(|at| arguments.get(at + 1)).cloned();
    let store = value("--store").map(PathBuf::from).unwrap_or_else(default_store);
    let options = GraphOptions {
        output: value("--out").map(PathBuf::from).unwrap_or_else(|| graph_path(&store)),
        store,
        journals: value("--journals").map(PathBuf::from).unwrap_or_else(default_journals),
        courts: value("--courts").map(|courts| courts.split(',').map(str::to_owned).collect()).unwrap_or_default(),
        limit: value("--limit").and_then(|limit| limit.parse().ok()),
        threads: value("--threads").and_then(|threads| threads.parse().ok()).unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(4, |count| (count.get() / 2).max(1))
        }),
    };
    match build_citator_graph(&options) {
        Ok(summary) => println!("Note-up graph written to {}\n{}", options.output.display(),
            serde_json::to_string_pretty(&summary).unwrap_or_default()),
        Err(error) => { eprintln!("error: {error}"); std::process::exit(1); }
    }
}
