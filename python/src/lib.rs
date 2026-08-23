use legal_structure_core::{
    a2aj_document_structure, pair_journal_footnotes as pair_journal, A2ajInput, A2ajSourceKind,
    DocumentBlock, DocumentKind, DocumentOrigin, DocumentQuery, DocumentStructure, ScalarText,
};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyDictMethods, PyList, PyListMethods};
use pyo3::IntoPyObjectExt;

#[pyfunction]
fn pair_journal_footnotes<'py>(
    py: Python<'py>,
    text: &str,
    page_labels: Vec<String>,
) -> PyResult<Bound<'py, PyDict>> {
    let result = py.detach(|| pair_journal(text, &page_labels));
    let notes = PyList::empty(py);
    for note in result.notes {
        let item = PyDict::new(py);
        item.set_item("label", note.label)?;
        item.set_item("restart_sequence", note.restart_sequence)?;
        item.set_item("note_page_index", note.note_page_index)?;
        item.set_item("ref_page_index", note.ref_page_index)?;
        item.set_item("body", note.body)?;
        item.set_item("truncated", note.truncated)?;
        item.set_item("proposition", note.proposition)?;
        item.set_item("passage", note.passage)?;
        notes.append(item)?;
    }
    let output = PyDict::new(py);
    output.set_item("notes", notes)?;
    for (name, value) in [
        ("symbol_labels_dropped", result.symbol_labels_dropped),
        ("labels_candidates", result.labels_candidates),
        ("labels_selected", result.labels_selected),
        ("refs_assigned", result.refs_assigned),
        ("ambiguous_sites", result.ambiguous_sites),
        ("crossrefs", result.crossrefs),
        ("crossrefs_unresolved", result.crossrefs_unresolved),
        ("pages", result.pages),
    ] {
        output.set_item(name, value)?;
    }
    output.set_item("footnote_mode", result.footnote_mode)?;
    Ok(output)
}

fn origin_name(origin: DocumentOrigin) -> &'static str {
    match origin {
        DocumentOrigin::Native => "native",
        DocumentOrigin::Heuristic => "heuristic",
    }
}

fn parse_kind(value: &str) -> PyResult<DocumentKind> {
    match value {
        "paragraph" => Ok(DocumentKind::Paragraph),
        "page" => Ok(DocumentKind::Page),
        "section" => Ok(DocumentKind::Section),
        _ => Err(PyValueError::new_err(
            "kind must be paragraph, page, or section",
        )),
    }
}

fn text_slice(text: &[u16], start: usize, end: usize) -> PyResult<String> {
    let units = text
        .get(start..end)
        .ok_or_else(|| PyRuntimeError::new_err("document block is outside its text"))?;
    String::from_utf16(units)
        .map_err(|_| PyRuntimeError::new_err("document block splits a Unicode character"))
}

fn numbered<'py>(py: Python<'py>, label: Option<String>) -> PyResult<Bound<'py, PyAny>> {
    let Some(label) = label else {
        return Ok(py.None().into_bound(py));
    };
    let value = label
        .strip_prefix("par")
        .or_else(|| label.strip_prefix("page"))
        .unwrap_or(&label);
    if let Ok(number) = value.parse::<u64>() {
        number.into_bound_py_any(py)
    } else if let Ok(number) = value.parse::<f64>() {
        if number.is_finite() {
            number.into_bound_py_any(py)
        } else {
            label.into_bound_py_any(py)
        }
    } else {
        label.into_bound_py_any(py)
    }
}

#[pyclass(frozen)]
struct Document {
    structure: DocumentStructure,
    query: DocumentQuery,
}

impl Document {
    fn primary(&self) -> Option<(&'static str, DocumentKind)> {
        [
            ("paragraphs", DocumentKind::Paragraph),
            ("pages", DocumentKind::Page),
            ("sections", DocumentKind::Section),
        ]
        .into_iter()
        .find(|(_, kind)| self.top_blocks(*kind).next().is_some())
    }

    fn top_blocks(&self, kind: DocumentKind) -> impl Iterator<Item = DocumentBlock> + '_ {
        self.query
            .blocks(&self.structure, Some(kind))
            .filter(|block| block.parent_label.is_none())
    }
}

#[pymethods]
impl Document {
    #[new]
    #[pyo3(signature = (
        doc_type,
        citation,
        text,
        *,
        alternate_citation=None,
        dataset=None,
        name=None,
        section_map=None
    ))]
    fn new(
        py: Python<'_>,
        doc_type: &str,
        citation: String,
        text: String,
        alternate_citation: Option<String>,
        dataset: Option<String>,
        name: Option<String>,
        section_map: Option<Vec<(String, String)>>,
    ) -> PyResult<Self> {
        let source_kind = match doc_type {
            "cases" => A2ajSourceKind::Cases,
            "laws" => A2ajSourceKind::Laws,
            _ => return Err(PyValueError::new_err("doc_type must be cases or laws")),
        };
        let mut input = A2ajInput::new(
            citation,
            source_kind,
            if section_map.is_some() {
                String::new()
            } else {
                text
            },
        );
        input.dataset = dataset;
        input.name = name;
        input.alternate_citation = alternate_citation;
        input.section_map = section_map;
        let structure = py
            .detach(|| a2aj_document_structure(input))
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        Ok(Self {
            structure,
            query: DocumentQuery::new(),
        })
    }

    #[getter]
    fn kind(&self) -> &'static str {
        self.primary().map_or("none", |(name, _)| name)
    }

    #[getter]
    fn count(&self) -> usize {
        self.primary()
            .map(|(_, kind)| self.top_blocks(kind).count())
            .unwrap_or_default()
    }

    #[getter]
    fn first<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        numbered(
            py,
            self.primary()
                .and_then(|(_, kind)| self.top_blocks(kind).next())
                .map(|block| block.label),
        )
    }

    #[getter]
    fn last<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        numbered(
            py,
            self.primary()
                .and_then(|(_, kind)| self.top_blocks(kind).last())
                .map(|block| block.label),
        )
    }

    #[getter]
    fn span(&self) -> f64 {
        let Some((_, kind)) = self.primary() else {
            return 0.0;
        };
        let mut blocks = self.top_blocks(kind);
        let Some(first) = blocks.next() else {
            return 0.0;
        };
        let Some(last) = blocks.last() else {
            return 1.0;
        };
        let value = (last.start - first.start) as f64
            / self.structure.query_text().encode_utf16().count().max(1) as f64;
        (value * 10_000.0).round() / 10_000.0
    }

    fn blocks(&self, kind: &str) -> PyResult<Vec<(String, Vec<String>, usize, usize)>> {
        let kind = parse_kind(kind)?;
        let coordinates = ScalarText::new(self.structure.query_text());
        self.top_blocks(kind)
            .map(|block| {
                Ok((
                    block.label.clone(),
                    block.aliases.clone(),
                    coordinates.scalar_at_utf16(block.start).ok_or_else(|| {
                        PyRuntimeError::new_err("document block starts inside a Unicode character")
                    })?,
                    coordinates.scalar_at_utf16(block.end).ok_or_else(|| {
                        PyRuntimeError::new_err("document block ends inside a Unicode character")
                    })?,
                ))
            })
            .collect()
    }

    fn segments(&self) -> PyResult<Vec<(String, Option<String>, Option<String>, String)>> {
        let text = self
            .structure
            .query_text()
            .encode_utf16()
            .collect::<Vec<_>>();
        let mut blocks = self
            .primary()
            .map(|(_, kind)| self.top_blocks(kind).collect::<Vec<_>>())
            .unwrap_or_default();
        blocks.sort_by_key(|block| block.start);
        let mut segments = Vec::with_capacity(blocks.len() * 2 + 1);
        let mut cursor = 0;
        for block in blocks {
            if block.start > cursor {
                segments.push((
                    "text".to_owned(),
                    None,
                    None,
                    text_slice(&text, cursor, block.start)?,
                ));
            }
            let kind = match block.kind {
                DocumentKind::Paragraph => "paragraph",
                DocumentKind::Page => "page",
                DocumentKind::Section => "section",
                _ => unreachable!("primary blocks are paragraphs, pages, or sections"),
            };
            segments.push((
                kind.to_owned(),
                Some(block.label.clone()),
                Some(origin_name(block.origin).to_owned()),
                text_slice(&text, block.start, block.end)?,
            ));
            cursor = cursor.max(block.end);
        }
        if cursor < text.len() || segments.is_empty() {
            segments.push((
                "text".to_owned(),
                None,
                None,
                text_slice(&text, cursor, text.len())?,
            ));
        }
        Ok(segments)
    }
}

#[pymodule]
fn legal_structure(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<Document>()?;
    module.add_function(wrap_pyfunction!(pair_journal_footnotes, module)?)
}
