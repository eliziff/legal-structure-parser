use crate::{DocumentStructure, ScalarRange};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{self, Write};

const COMPONENTS: [&str; 12] = [
    "identity",
    "text",
    "nodes",
    "document",
    "notes",
    "authorities",
    "definitions",
    "docx",
    "contents",
    "crossReferences",
    "diagnostics",
    "selection",
];

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentFingerprint {
    pub schema_version: &'static str,
    pub result_sha256: String,
    pub components: BTreeMap<&'static str, String>,
    pub counts: DocumentFingerprintCounts,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentFingerprintCounts {
    pub nodes: usize,
    pub notes: usize,
    pub authorities: usize,
    pub definitions: usize,
    pub diagnostics: usize,
}

#[derive(Default)]
struct DigestWriter(Sha256);

impl Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialization_sha256(value: &impl Serialize) -> String {
    let mut writer = DigestWriter::default();
    serde_json::to_writer(&mut writer, value).expect("fingerprint serialization");
    format!("{:x}", writer.0.finalize())
}

pub fn document_fingerprint(document: &DocumentStructure) -> DocumentFingerprint {
    let text_sha256 = format!("{:x}", Sha256::digest(document.text.as_bytes()));
    let query_text_sha256 = document
        .rendered_text
        .as_deref()
        .map(|text| format!("{:x}", Sha256::digest(text.as_bytes())))
        .unwrap_or_else(|| text_sha256.clone());
    let rendered_ranges = document
        .nodes
        .iter()
        .map(|node| node.rendered_range)
        .collect::<Vec<Option<ScalarRange>>>();
    let nodes = document
        .nodes
        .iter()
        .map(|node| {
            (
                (
                    &node.id,
                    &node.kind,
                    &node.range,
                    &node.origin_id,
                    &node.source,
                    &node.label,
                    &node.locator_kind,
                    &node.aliases,
                    &node.parent_id,
                    &node.anchor,
                ),
                (
                    &node.content_start,
                    &node.marker_range,
                    &node.row_span,
                    &node.column_span,
                    &node.display_value,
                    &node.page_indexes,
                    &node.line_ids,
                    &node.grammar,
                    &node.proof,
                ),
            )
        })
        .collect::<Vec<_>>();
    let components = BTreeMap::from([
        (
            "identity",
            serialization_sha256(&(
                &document.schema_version,
                &document.document_id,
                &document.offset_unit,
                &document.provider,
                &document.url,
                &document.doc_type,
                &document.profile,
                &document.revision,
                &document.text_sha256,
                &document.source_sha256,
                &document.scope,
                &document.origins,
            )),
        ),
        ("text", text_sha256),
        ("nodes", serialization_sha256(&nodes)),
        (
            "document",
            serialization_sha256(&(query_text_sha256, rendered_ranges)),
        ),
        ("notes", serialization_sha256(&document.notes)),
        (
            "authorities",
            serialization_sha256(&document.cited_authorities),
        ),
        ("definitions", serialization_sha256(&document.definitions)),
        ("docx", serialization_sha256(&document.docx)),
        ("contents", serialization_sha256(&document.contents)),
        (
            "crossReferences",
            serialization_sha256(&document.cross_references),
        ),
        ("diagnostics", serialization_sha256(&document.diagnostics)),
        (
            "selection",
            serialization_sha256(&document.selected_hypothesis),
        ),
    ]);
    let result_sha256 =
        serialization_sha256(&COMPONENTS.map(|name| (name, components[name].as_str())));
    let counts = DocumentFingerprintCounts {
        nodes: document.nodes.len(),
        notes: document.notes.len(),
        authorities: document.cited_authorities.len(),
        definitions: document.definitions.len(),
        diagnostics: document.diagnostics.len(),
    };
    DocumentFingerprint {
        schema_version: "legalpdf.document-fingerprint.v1",
        result_sha256,
        components,
        counts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Derivation, DocumentStructure, NodeKind, Scope, StructureNode};

    #[test]
    fn rendered_plane_changes_document_component_only() {
        let node = StructureNode::new(
            "par1".into(),
            NodeKind::Paragraph,
            ScalarRange { start: 0, end: 3 },
            "test",
            Derivation::Native,
            None,
        );
        let mut document = DocumentStructure::from_scalar_parts(
            "id".into(),
            "raw".into(),
            format!("{:x}", Sha256::digest(b"raw")),
            None,
            Scope::complete(),
            Vec::new(),
            vec![node],
            Vec::new(),
            Vec::new(),
        );
        let before = document_fingerprint(&document);
        document.rendered_text = Some("rendered".into());
        document.nodes[0].rendered_range = Some(ScalarRange { start: 0, end: 8 });
        let after = document_fingerprint(&document);
        assert_ne!(before.components["document"], after.components["document"]);
        assert_eq!(before.components["nodes"], after.components["nodes"]);
        document.text = "changed raw witness".into();
        assert_ne!(
            after.components["text"],
            document_fingerprint(&document).components["text"]
        );
    }
}
