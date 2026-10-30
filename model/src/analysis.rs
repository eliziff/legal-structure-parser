use crate::{
    AuthoritativeTableCell, CandidateEvidenceV2, CandidateRoleV2, DocumentStructure, EngineError,
    NotePairClaimV2, StructureCandidateRun, StructureDiagnostic, StructureNode,
};
use legal_layout::LayoutAnalysis;

/// The structure engine's document reading, as parsers that build documents on the model call it,
/// beside its services on laid-out text. A parser takes this as a dependency on the model alone, so
/// editing the engine recompiles the engine and the program that supplies it, not every parser.
pub trait StructureAnalysis: LayoutAnalysis {
    /// The numbering runs in `text`: its counts of numbers, letters and bullets, and the provision
    /// ladder the instrument grammar reads. With `refinements`, the ladder also reads a section's
    /// number its subsections repeat ("33(2) The …"), and leaves out enactment history notes,
    /// regnal heads, contents columns and years standing alone.
    fn detect_structure_candidate_runs(&self, text: &str, refinements: bool) -> Vec<StructureCandidateRun>;

    /// Each candidate of `runs` resolved, in text order, to the unit it opens, if any: its evidence
    /// says whether it numbers anything, its run's numbering family what.
    fn resolve_candidates(
        &self,
        runs: &[StructureCandidateRun],
        evidence: &[CandidateEvidenceV2],
    ) -> Result<Vec<CandidateRoleV2>, EngineError>;

    /// Bind the producer's nodes, the candidates in their `roles` and the note pairs into one
    /// graph, then invoke the producer's single outline assembler. `evidence` gives each candidate
    /// its lines, pages and observations. The producer refreshes the revision once it completes
    /// the structure.
    #[allow(clippy::too_many_arguments)]
    fn resolve_structure_graph_with_assembler(
        &self,
        document_id: String,
        provider: String,
        text: &str,
        source_sha256: Option<String>,
        nodes: Vec<StructureNode>,
        runs: &[StructureCandidateRun],
        evidence: &[CandidateEvidenceV2],
        roles: &[CandidateRoleV2],
        note_pairs: &[NotePairClaimV2],
        diagnostics: Vec<StructureDiagnostic>,
        assembler: &mut dyn FnMut(&mut [StructureNode]),
    ) -> Result<DocumentStructure, EngineError>;

    fn analyze_instrument(
        &self,
        text: String,
        document_id: String,
        table_cells: &[AuthoritativeTableCell],
        reconstruct_lineation: bool,
    ) -> Result<DocumentStructure, EngineError>;

    fn analyze_docx(
        &self,
        document_id: String,
        paragraphs: Vec<String>,
        table_cells: &[AuthoritativeTableCell],
    ) -> Result<DocumentStructure, EngineError>;
}

static STRUCTURE_ANALYSIS: std::sync::OnceLock<&'static dyn StructureAnalysis> = std::sync::OnceLock::new();

/// Supplies the structure engine's document reading to the parsers built on the model. The first
/// one supplied stays.
pub fn install_structure_analysis(analysis: &'static dyn StructureAnalysis) {
    let _ = STRUCTURE_ANALYSIS.set(analysis);
}

/// The supplied structure engine.
pub fn structure_analysis() -> &'static dyn StructureAnalysis {
    *STRUCTURE_ANALYSIS
        .get()
        .expect("the program must supply the structure engine with install_structure_analysis")
}
