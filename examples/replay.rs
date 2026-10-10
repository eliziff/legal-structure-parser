//! Replays the structure engine over the inputs the PDF parser dumped (LEGALPDF_RESOLVER_DUMP) and
//! writes each document's structure, so the structure gold scores an engine change without parsing
//! (experiments/legal-structure-gold/score_all.py --replay). replay JOBS.json [THREADS]
//! JOBS.json: [{"dump": ..., "output": ...}]

use legal_structure_model::{
    CandidateEvidenceV2, NotePairClaimV2, StructureAnalysis, StructureCandidateRun,
    StructureDiagnostic, StructureNode,
};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

#[derive(Deserialize)]
struct Job {
    dump: PathBuf,
    output: PathBuf,
}

#[derive(Deserialize)]
struct Dump {
    document_id: String,
    source_sha256: String,
    text: String,
    nodes: Vec<StructureNode>,
    runs: Vec<StructureCandidateRun>,
    evidence: Vec<CandidateEvidenceV2>,
    note_pairs: Vec<NotePairClaimV2>,
    diagnostics: Vec<StructureDiagnostic>,
}

fn replay(job: &Job) -> Result<(), String> {
    let bytes = std::fs::read(&job.dump).map_err(|e| e.to_string())?;
    let dump: Dump = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let structure = legal_structure::STRUCTURE_ANALYSIS
        .resolve_structure_graph(
            dump.document_id,
            "local-pdf".to_owned(),
            &dump.text,
            Some(dump.source_sha256),
            dump.nodes,
            &dump.runs,
            &dump.evidence,
            &dump.note_pairs,
            dump.diagnostics,
        )
        .map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec(&structure).map_err(|e| e.to_string())?;
    std::fs::write(&job.output, bytes).map_err(|e| e.to_string())
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let jobs: Vec<Job> =
        serde_json::from_slice(&std::fs::read(&arguments[0]).expect("jobs")).expect("jobs json");
    let threads = arguments.get(1).and_then(|n| n.parse().ok()).unwrap_or(8);
    let started = Instant::now();
    let next = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(job) = jobs.get(index) else { break };
                if let Err(error) = replay(job) {
                    failed.fetch_add(1, Ordering::Relaxed);
                    eprintln!("{}: {error}", job.dump.display());
                }
            });
        }
    });
    eprintln!(
        "replayed {} documents, {} failed, in {:.1}s",
        jobs.len(),
        failed.load(Ordering::Relaxed),
        started.elapsed().as_secs_f64()
    );
}
