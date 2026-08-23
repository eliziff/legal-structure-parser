use crate::{
    derive_docx_numbering, javascript_whitespace, text::JS_WHITESPACE_CLASS, utf16_len,
    DocxNumberAnchor,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

const LABELS: [&str; 5] = ["Schedule", "Exhibit", "Appendix", "Annex", "Annexure"];
const MAX_FINDINGS: usize = 200;
const EXCERPT_LENGTH: usize = 160;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DocxLintCode {
    CrossReferenceMissing,
    AttachmentReferenceMissing,
    NumberingGap,
    NumberingDuplicate,
    DefinedTermDuplicate,
    DefinedTermUnused,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DocxLintSeverity {
    Error,
    Warning,
}

#[derive(Debug, Serialize)]
pub struct DocxLintFinding {
    pub code: DocxLintCode,
    pub severity: DocxLintSeverity,
    pub subject: String,
    pub message: String,
    pub paragraph_index: usize,
    pub excerpt: String,
}

#[derive(Debug, Serialize)]
pub struct DocxLintReferenceChecks {
    pub references: usize,
    pub resolved: usize,
    pub skipped_external: usize,
}

#[derive(Debug, Serialize)]
pub struct DocxLintAttachmentChecks {
    pub references: usize,
    pub resolved: usize,
}

#[derive(Debug, Serialize)]
pub struct DocxLintCount {
    pub anchors: usize,
}

#[derive(Debug, Serialize)]
pub struct DocxLintDefinitionCount {
    pub definitions: usize,
}

#[derive(Debug, Serialize)]
pub struct DocxLintChecks {
    pub cross_references: DocxLintReferenceChecks,
    pub attachments: DocxLintAttachmentChecks,
    pub numbering: DocxLintCount,
    pub defined_terms: DocxLintDefinitionCount,
}

#[derive(Debug, Serialize)]
pub struct DocxLintReport {
    pub paragraphs: usize,
    pub checks: DocxLintChecks,
    pub findings: Vec<DocxLintFinding>,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DocxCrossReferenceStatus {
    Resolved,
    SkippedExternal,
    MissingRomanArticle,
    MissingSibling { parent: String },
    MissingTopLevel,
    Abstained,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocxCrossReference {
    pub paragraph_index: usize,
    pub subject: String,
    pub value: String,
    pub status: DocxCrossReferenceStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DocxAttachmentReferenceStatus {
    Resolved,
    Missing { included: Vec<String> },
    AbstainedNoAnchor,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocxAttachmentReference {
    pub paragraph_index: usize,
    pub label: String,
    pub id: String,
    pub subject: String,
    pub status: DocxAttachmentReferenceStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocxStructureFacts {
    pub numbering: crate::DocxNumberingResult,
    pub cross_references: Vec<DocxCrossReference>,
    pub attachments: Vec<DocxAttachmentReference>,
}

fn reference_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(&format!(
        r"(?-u:\b)(Section|Sections|Clause|Clauses|Article|Articles|Paragraph|Paragraphs){JS_WHITESPACE_CLASS}+(\d{{1,3}}(?:\.\d{{1,3}})*|[IVXLCDM]+)(?-u:\b)"
    )).unwrap())
}

fn external_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(&format!(
            r"(?i)^{JS_WHITESPACE_CLASS}*(?:\([a-z0-9]{{1,4}}\){JS_WHITESPACE_CLASS}*)?(?:of|to|under){JS_WHITESPACE_CLASS}+((?-u:\w+))"
        ))
        .unwrap()
    })
}

fn is_external(following: &str) -> bool {
    external_pattern()
        .captures(following)
        .and_then(|captures| captures.get(1))
        .is_some_and(|owner| !owner.as_str().eq_ignore_ascii_case("this"))
}

fn cross_references(
    paragraphs: &[String],
    numbers: &[DocxNumberAnchor],
    romans: &[DocxNumberAnchor],
) -> Vec<DocxCrossReference> {
    let anchors = numbers
        .iter()
        .map(|anchor| anchor.number.as_str())
        .collect::<HashSet<_>>();
    let ancestor_prefixes = anchors
        .iter()
        .flat_map(|anchor| anchor.match_indices('.').map(|(at, _)| &anchor[..at]))
        .collect::<HashSet<_>>();
    let roman_anchors = romans
        .iter()
        .map(|anchor| anchor.number.as_str())
        .collect::<HashSet<_>>();
    let child_depths = anchors
        .iter()
        .filter_map(|anchor| {
            let (parent, _) = anchor.rsplit_once('.')?;
            Some((parent, anchor.matches('.').count() + 1))
        })
        .collect::<HashSet<_>>();
    let top_levels = anchors
        .iter()
        .filter(|anchor| !anchor.contains('.'))
        .count();
    let mut facts = Vec::new();
    for (paragraph_index, paragraph) in paragraphs.iter().enumerate() {
        for captures in reference_pattern().captures_iter(paragraph) {
            let whole = captures.get(0).unwrap();
            let value = captures.get(2).unwrap().as_str();
            let roman = value.bytes().all(|byte| b"IVXLCDM".contains(&byte));
            let status = if is_external(&paragraph[whole.end()..]) {
                DocxCrossReferenceStatus::SkippedExternal
            } else if roman {
                if roman_anchors.is_empty() {
                    DocxCrossReferenceStatus::Abstained
                } else if roman_anchors.contains(value) {
                    DocxCrossReferenceStatus::Resolved
                } else {
                    DocxCrossReferenceStatus::MissingRomanArticle
                }
            } else if anchors.contains(value) || ancestor_prefixes.contains(value) {
                DocxCrossReferenceStatus::Resolved
            } else if let Some((parent, _)) = value.rsplit_once('.') {
                if child_depths.contains(&(parent, value.matches('.').count() + 1)) {
                    DocxCrossReferenceStatus::MissingSibling {
                        parent: parent.into(),
                    }
                } else {
                    DocxCrossReferenceStatus::Abstained
                }
            } else if top_levels >= 3 {
                DocxCrossReferenceStatus::MissingTopLevel
            } else {
                DocxCrossReferenceStatus::Abstained
            };
            facts.push(DocxCrossReference {
                paragraph_index,
                subject: whole.as_str().into(),
                value: value.into(),
                status,
            });
        }
    }
    facts
}

fn attachment_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        let labels = LABELS.join("|");
        Regex::new(&format!(
            r"(?:(?i:^({labels}){JS_WHITESPACE_CLASS}+(\d{{1,3}}|[A-Z]{{1,3}})(?-u:\b))|(?-u:\b)({labels})s?{JS_WHITESPACE_CLASS}+(\d{{1,3}}|[A-Z]{{1,3}})(?-u:\b))"
        ))
        .unwrap()
    })
}

fn heading_like(text: &str, end: usize) -> bool {
    utf16_len(text) <= 80
        || text[..end].to_uppercase() == text[..end]
        || text[end..]
            .trim_start_matches(javascript_whitespace)
            .chars()
            .next()
            .is_some_and(|character| matches!(character, '-' | '–' | '—' | ':' | '.'))
}

fn attachments(paragraphs: &[String]) -> Vec<DocxAttachmentReference> {
    let mut anchors = HashMap::<&str, HashSet<String>>::new();
    let mut found = Vec::<DocxAttachmentReference>::new();
    for (paragraph_index, text) in paragraphs.iter().enumerate() {
        let mut anchor_label = None;
        let mut references = attachment_pattern()
            .captures_iter(text)
            .filter_map(|capture| {
                let whole = capture.get(0).unwrap();
                if let Some(id) = capture.get(2) {
                    if heading_like(text, whole.end()) {
                        let label = LABELS
                            .iter()
                            .find(|label| capture[1].eq_ignore_ascii_case(label))?;
                        anchor_label = Some(*label);
                        anchors
                            .entry(*label)
                            .or_default()
                            .insert(id.as_str().to_uppercase());
                    }
                    return None;
                }
                if whole.start() == 0 || is_external(&text[whole.end()..]) {
                    return None;
                }
                let label = LABELS.iter().find(|label| capture[3] == ***label)?;
                (Some(*label) != anchor_label).then(|| DocxAttachmentReference {
                    paragraph_index,
                    label: (*label).into(),
                    id: capture[4].to_uppercase(),
                    subject: whole.as_str().into(),
                    status: DocxAttachmentReferenceStatus::AbstainedNoAnchor,
                })
            })
            .collect::<Vec<_>>();
        references.sort_by_key(|reference| {
            LABELS
                .iter()
                .position(|label| *label == reference.label)
                .unwrap()
        });
        found.extend(references);
    }
    let mut label_order = HashMap::<String, usize>::new();
    for reference in &found {
        let next = label_order.len();
        label_order.entry(reference.label.clone()).or_insert(next);
    }
    found.sort_by_key(|reference| label_order[&reference.label]);
    for reference in &mut found {
        let Some(included) = anchors
            .get(reference.label.as_str())
            .filter(|set| !set.is_empty())
        else {
            continue;
        };
        reference.status = if included.contains(&reference.id) {
            DocxAttachmentReferenceStatus::Resolved
        } else {
            let mut included = included.iter().cloned().collect::<Vec<_>>();
            included.sort();
            DocxAttachmentReferenceStatus::Missing { included }
        };
    }
    found
}

fn derive_docx_lint_facts(paragraphs: &[String]) -> DocxStructureFacts {
    let numbering = derive_docx_numbering(paragraphs);
    DocxStructureFacts {
        cross_references: cross_references(
            paragraphs,
            &numbering.number_anchors,
            &numbering.roman_article_anchors,
        ),
        attachments: attachments(paragraphs),
        numbering,
    }
}

fn excerpt_around(text: &str, subject: &str) -> String {
    let text = crate::text::ScalarText::new(text);
    let subject_len = utf16_len(subject);
    let Some(index) = text
        .value
        .find(subject)
        .and_then(|byte| text.utf16_at_byte(byte))
    else {
        let end = text.utf16_len().min(EXCERPT_LENGTH);
        return text.value[..text.byte_at_utf16_floor(end).unwrap()].to_owned();
    };
    let start = index.saturating_sub(EXCERPT_LENGTH.saturating_sub(subject_len) / 2);
    let end = (start + EXCERPT_LENGTH).min(text.utf16_len());
    let slice = &text.value
        [text.byte_at_utf16_floor(start).unwrap()..text.byte_at_utf16_floor(end).unwrap()];
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        slice,
        if end < text.utf16_len() { "…" } else { "" }
    )
}

pub fn docx_structure_lint(
    structure: &crate::DocumentStructure,
) -> Result<DocxLintReport, crate::EngineError> {
    let facts = structure
        .docx
        .as_ref()
        .ok_or_else(|| crate::EngineError::invalid("DOCX structure facts are missing"))?;
    let paragraphs = structure.text.split('\n').collect::<Vec<_>>();
    let excerpt = |paragraph: usize, subject: &str| {
        excerpt_around(paragraphs.get(paragraph).copied().unwrap_or(""), subject)
    };
    let mut findings = Vec::new();
    let mut notes = Vec::new();

    for reference in &facts.cross_references {
        let message = match &reference.status {
            DocxCrossReferenceStatus::MissingRomanArticle => Some(format!(
                "{} is referenced but no Article {} heading exists in this document.",
                reference.subject, reference.value
            )),
            DocxCrossReferenceStatus::MissingTopLevel => Some(format!(
                "{} is referenced but no provision {} exists in this document.",
                reference.subject, reference.value
            )),
            DocxCrossReferenceStatus::MissingSibling { parent } => Some(format!(
                "{} is referenced but does not exist; sibling provisions under {} are numbered without it.",
                reference.subject, parent
            )),
            _ => None,
        };
        if let Some(message) = message {
            findings.push(DocxLintFinding {
                code: DocxLintCode::CrossReferenceMissing,
                severity: DocxLintSeverity::Error,
                subject: reference.subject.clone(),
                message,
                paragraph_index: reference.paragraph_index,
                excerpt: excerpt(reference.paragraph_index, &reference.subject),
            });
        }
    }

    let skipped_external = facts
        .cross_references
        .iter()
        .filter(|reference| reference.status == DocxCrossReferenceStatus::SkippedExternal)
        .count();
    let abstained = facts
        .cross_references
        .iter()
        .filter(|reference| reference.status == DocxCrossReferenceStatus::Abstained)
        .count();
    if facts.numbering.number_anchors.is_empty()
        && facts.numbering.roman_article_anchors.is_empty()
        && facts.cross_references.len() > skipped_external
    {
        notes.push("Internal cross-references were found but the document has no literal clause numbering to check them against (Word field numbering is not resolved by this lint); cross-reference checks abstained.".into());
    } else if abstained > 0 {
        notes.push(format!(
            "{abstained} cross-reference(s) could not be checked against the document's numbering scheme and were not flagged."
        ));
    }

    let mut attachment_labels = HashMap::<&str, usize>::new();
    for attachment in &facts.attachments {
        match &attachment.status {
            DocxAttachmentReferenceStatus::AbstainedNoAnchor => {
                *attachment_labels.entry(&attachment.label).or_default() += 1;
            }
            DocxAttachmentReferenceStatus::Missing { included } => {
                findings.push(DocxLintFinding {
                    code: DocxLintCode::AttachmentReferenceMissing,
                    severity: DocxLintSeverity::Error,
                    subject: attachment.subject.clone(),
                    message: format!(
                        "{} is referenced but only {} {} {} included in this document.",
                        attachment.subject,
                        attachment.label,
                        included.join(", "),
                        if included.len() == 1 { "is" } else { "are" }
                    ),
                    paragraph_index: attachment.paragraph_index,
                    excerpt: excerpt(attachment.paragraph_index, &attachment.subject),
                });
            }
            DocxAttachmentReferenceStatus::Resolved => {}
        }
    }
    for (label, count) in attachment_labels {
        notes.push(format!(
            "{count} {label} reference(s) found but no {label} is included in this document (attachments may be separate files); not checked."
        ));
    }

    let mut numbering_findings = facts
        .numbering
        .duplicates
        .iter()
        .map(|duplicate| DocxLintFinding {
            code: DocxLintCode::NumberingDuplicate,
            severity: DocxLintSeverity::Warning,
            subject: duplicate.number.clone(),
            message: format!(
                "Provision number {} appears more than once.",
                duplicate.number
            ),
            paragraph_index: duplicate.paragraph_index,
            excerpt: excerpt(duplicate.paragraph_index, &duplicate.number),
        })
        .chain(facts.numbering.gaps.iter().map(|gap| DocxLintFinding {
            code: DocxLintCode::NumberingGap,
            severity: DocxLintSeverity::Warning,
            subject: gap.number.clone(),
            message: format!(
                "Numbering jumps from {} to {}; {}.",
                gap.previous_number,
                gap.number,
                if gap.missing.len() == 1 {
                    format!("{} is missing", gap.missing[0])
                } else {
                    format!("{} provisions are missing in between", gap.missing.len())
                }
            ),
            paragraph_index: gap.paragraph_index,
            excerpt: excerpt(gap.paragraph_index, &gap.number),
        }))
        .collect::<Vec<_>>();
    numbering_findings.sort_by_key(|finding| finding.paragraph_index);
    findings.extend(numbering_findings);

    if structure.definitions.is_empty() {
        notes.push("No quoted defined terms were detected; defined-term checks abstained.".into());
    }
    for term in &structure.definitions {
        let defined_in = term
            .definitions
            .iter()
            .filter_map(|definition| definition.source_paragraph_id.parse::<usize>().ok())
            .collect::<Vec<_>>();
        if defined_in.len() > 1 {
            let paragraph = *defined_in.last().unwrap();
            findings.push(DocxLintFinding {
                code: DocxLintCode::DefinedTermDuplicate,
                severity: DocxLintSeverity::Warning,
                subject: term.term.clone(),
                message: format!(
                    "\"{}\" is defined {} times (paragraphs {}).",
                    term.term,
                    defined_in.len(),
                    defined_in
                        .iter()
                        .map(usize::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                paragraph_index: paragraph,
                excerpt: excerpt(paragraph, &term.term),
            });
        }
        if term.uses.is_empty() {
            if let Some(&paragraph) = defined_in.first() {
                findings.push(DocxLintFinding {
                    code: DocxLintCode::DefinedTermUnused,
                    severity: DocxLintSeverity::Warning,
                    subject: term.term.clone(),
                    message: format!(
                        "\"{}\" is defined but never used elsewhere in this document.",
                        term.term
                    ),
                    paragraph_index: paragraph,
                    excerpt: excerpt(paragraph, &term.term),
                });
            }
        }
    }

    if findings.len() > MAX_FINDINGS {
        notes.push(format!(
            "Findings truncated to {MAX_FINDINGS} of {}.",
            findings.len()
        ));
    }
    findings.truncate(MAX_FINDINGS);
    Ok(DocxLintReport {
        paragraphs: paragraphs.len(),
        checks: DocxLintChecks {
            cross_references: DocxLintReferenceChecks {
                references: facts.cross_references.len(),
                resolved: facts
                    .cross_references
                    .iter()
                    .filter(|reference| reference.status == DocxCrossReferenceStatus::Resolved)
                    .count(),
                skipped_external,
            },
            attachments: DocxLintAttachmentChecks {
                references: facts.attachments.len(),
                resolved: facts
                    .attachments
                    .iter()
                    .filter(|attachment| {
                        attachment.status == DocxAttachmentReferenceStatus::Resolved
                    })
                    .count(),
            },
            numbering: DocxLintCount {
                anchors: facts.numbering.number_anchors.len()
                    + facts.numbering.roman_article_anchors.len(),
            },
            defined_terms: DocxLintDefinitionCount {
                definitions: structure.definitions.len(),
            },
        },
        findings,
        notes,
    })
}

#[cfg(feature = "structure-inference")]
pub fn analyze_docx(
    document_id: String,
    paragraphs: Vec<String>,
    table_cells: &[crate::AuthoritativeTableCell],
) -> Result<crate::DocumentStructure, crate::EngineError> {
    let text = paragraphs.join("\n");
    let mut structure = crate::analyze_instrument(&text, document_id, table_cells, true)?;
    structure.provider = "docx".to_owned();
    structure.docx = Some(derive_docx_lint_facts(&paragraphs));
    Ok(structure)
}
