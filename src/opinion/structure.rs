//! The parse's nodes and readings, placed in the structure the product serves: the parts, notes, pages, paragraphs and
//! headings are its nodes, in the text's order; the rest is its `opinion` reading.

use super::*;
use crate::{DetectionProfile, Origin, Scope};
use sha2::{Digest, Sha256};

/// A record read whole, as the structure the product serves.
pub fn opinion_structure(record: &OpinionRecord, url: Option<String>) -> DocumentStructure {
    let layer = opinion_layer(record);
    let census = serde_json::json!({
        "failures": layer_failures(&layer), "screens": layer_screens(&layer, record),
        "page_witness": layer.page_witness, "note_witness": layer.note_witness, "heading_witness": layer.heading_witness,
        "paragraph_witness": layer.paragraph_witness, "paragraphs_run_on": layer.paragraphs_run_on, "paragraphs_split": layer.paragraphs_split,
        "union_test": layer.union_test, "union_samples": layer.union_samples, "titles_not_held": layer.titles_not_held,
        "marks_in_notes": layer.marks_in_notes(), "source_faults_left": layer.source_faults_left(), "unheld_text": layer.unheld_text(),
    });
    let mut nodes = Vec::new();
    for (name, list) in [("note", layer.notes), ("page", layer.pages), ("paragraph", layer.paragraphs), ("heading", layer.headings)] {
        nodes.extend(list.into_iter().enumerate().map(|(index, mut node)| {
            node.id = format!("opinion-{name}-{}", index + 1);
            node
        }));
    }
    nodes.extend(layer.parts);
    nodes.sort_by_key(|node| (node.range.start, std::cmp::Reverse(node.range.end)));
    let reading = OpinionReading {
        rendition: layer.rendition, citations: layer.citations, page_series: layer.page_series, headnotes: layer.headnotes,
        headnote_marks: layer.headnote_marks, blocks: layer.blocks, front: layer.front, furniture: layer.furniture,
        unmatched_calls: layer.unmatched_calls, tables: layer.tables, defined_terms: layer.defined_terms, quotes: layer.quotes,
        instruments: layer.instruments, record_gaps: layer.record_gaps, outcomes: layer.outcomes, document_kind: layer.document_kind,
        other_opinion: layer.other_opinion, census,
    };
    let text_sha256 = format!("{:x}", Sha256::digest(layer.text.as_bytes()));
    let mut structure = DocumentStructure::from_projected_parts(
        layer.id, "courtlistener".to_owned(), url, Some("cases".to_owned()), Some(DetectionProfile::CaseLossy), layer.text, text_sha256,
        None, Scope::complete(), vec![Origin { id: ORIGIN.to_owned() }], nodes, Vec::new(), Vec::new(),
    );
    structure.cited_authorities = layer.cited;
    structure.opinion = Some(Box::new(reading));
    structure.refresh_revision().expect("Typed document structure is serializable");
    structure
}
