use crate::{
    AuthoritativeTableCell, CandidateEvidenceV2, DocumentStructure, EngineError, NotePairClaimV2,
    StructureCandidateRun, StructureDiagnostic, StructureNode,
};
use legal_layout::LayoutAnalysis;

/// The structure engine's document reading, as parsers that build documents on the model call it,
/// beside its services on laid-out text. A parser takes this as a dependency on the model alone, so
/// editing the engine recompiles the engine and the program that supplies it, not every parser.
pub trait StructureAnalysis: LayoutAnalysis {
    fn detect_structure_candidate_runs(&self, text: &str) -> Vec<StructureCandidateRun>;

    #[allow(clippy::too_many_arguments)]
    fn resolve_structure_graph(
        &self,
        document_id: String,
        provider: String,
        text: &str,
        source_sha256: Option<String>,
        nodes: Vec<StructureNode>,
        runs: &[StructureCandidateRun],
        evidence: &[CandidateEvidenceV2],
        note_pairs: &[NotePairClaimV2],
        diagnostics: Vec<StructureDiagnostic>,
    ) -> Result<DocumentStructure, EngineError>;

    /// Resolve source candidates, then invoke the producer's single outline assembler. The
    /// producer refreshes the revision once it completes the structure.
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
