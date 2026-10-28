//! The grader's frame: a document as literal source-span blocks and annotations over its lines (the
//! gold's projection and a parse's alike), the documents that own them and the logical blocks their
//! continuations make. No classification runs here.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceSpan {
    pub line_id: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FrameLine {
    pub id: String,
    pub text: String,
    pub page: usize,
    #[serde(flatten)]
    pub evidence: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FrameBlock {
    #[serde(default)]
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub line_ids: Vec<String>,
    #[serde(default)]
    pub spans: Vec<SourceSpan>,
    #[serde(default)]
    pub attributes: Map<String, Value>,
    /// The document that owns the block, derived from the declared documents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FrameAnnotation {
    #[serde(default)]
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub line_ids: Vec<String>,
    #[serde(default)]
    pub spans: Vec<SourceSpan>,
    #[serde(default)]
    pub attributes: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FrameLink {
    pub next: String,
    pub separator: Option<String>,
    #[serde(flatten)]
    pub attributes: BTreeMap<String, Value>,
}

pub type FrameLinks = BTreeMap<String, BTreeMap<String, FrameLink>>;

#[derive(Debug, Deserialize)]
pub struct FrameRequest {
    pub lines: Vec<FrameLine>,
    pub groups: Vec<FrameBlock>,
    pub annotations: Vec<FrameAnnotation>,
    pub reading_order: Vec<String>,
    #[serde(default)]
    pub continuations: FrameLinks,
}

#[derive(Debug, Serialize)]
pub struct AssembledFrame {
    pub groups: Vec<FrameBlock>,
    pub annotations: Vec<FrameAnnotation>,
    /// Each logical block's fragments, keyed by its first.
    pub logical_blocks: BTreeMap<String, Vec<String>>,
}

pub(crate) fn anchor(span: &SourceSpan) -> String {
    format!("{}@{}", span.line_id, span.start)
}

pub(crate) fn spans(value: Option<&Value>) -> Result<Vec<SourceSpan>, String> {
    serde_json::from_value(value.cloned().unwrap_or_else(|| Value::Array(Vec::new()))).map_err(|e| e.to_string())
}

pub(crate) struct Source<'a> {
    pub(crate) lines: HashMap<&'a str, &'a FrameLine>,
    pub(crate) positions: HashMap<&'a str, usize>,
}

impl<'a> Source<'a> {
    pub(crate) fn new(lines: &'a [FrameLine], order: &'a [String]) -> Result<Self, String> {
        let source = Source {
            lines: lines.iter().map(|line| (line.id.as_str(), line)).collect(),
            positions: order.iter().enumerate().map(|(n, id)| (id.as_str(), n)).collect(),
        };
        if source.lines.len() != lines.len() || source.positions.len() != order.len()
            || source.lines.keys().any(|id| !source.positions.contains_key(id)) {
            return Err("Source identities and reading order must match exactly".into());
        }
        Ok(source)
    }

    /// An object's spans in reading order, whole lines where it names only lines.
    pub(crate) fn membership(&self, ids: &mut Vec<String>, spans: &mut Vec<SourceSpan>) -> Result<(), String> {
        if spans.is_empty() {
            *spans = ids.iter().map(|id| self.lines.get(id.as_str())
                .map(|line| SourceSpan { line_id: id.clone(), start: 0, end: line.text.chars().count() })
                .ok_or_else(|| "Membership names absent source".to_owned())).collect::<Result<_, _>>()?;
        }
        if spans.is_empty() || spans.iter().any(|span| !self.positions.contains_key(span.line_id.as_str())) {
            return Err("An object needs source membership".into());
        }
        spans.sort_by_key(|span| (self.positions[span.line_id.as_str()], span.start, span.end));
        let mut seen = HashSet::new();
        *ids = spans.iter().filter_map(|span| seen.insert(span.line_id.clone()).then(|| span.line_id.clone())).collect();
        Ok(())
    }

    pub(crate) fn canonical(&self, id: &str) -> String {
        if self.lines.contains_key(id) { format!("{id}@0") } else { id.to_owned() }
    }

    pub(crate) fn location(&self, id: &str) -> Result<(usize, usize), String> {
        if let Some(position) = self.positions.get(id) {
            return Ok((*position, 0));
        }
        let (line, offset) = id.rsplit_once('@').ok_or("Relationship needs a source-span anchor")?;
        let offset = offset.parse::<usize>().map_err(|_| "Invalid source-span anchor")?;
        Ok((*self.positions.get(line).ok_or("Relationship names absent source")?, offset))
    }

    fn links(&self, layer: Option<&BTreeMap<String, FrameLink>>) -> BTreeMap<String, String> {
        layer.into_iter().flatten().map(|(id, link)| (self.canonical(id), self.canonical(&link.next))).collect()
    }

    fn holds(&self, span: &SourceSpan, at: (usize, usize)) -> bool {
        self.positions[span.line_id.as_str()] == at.0 && span.start <= at.1 && at.1 < span.end
    }
}

pub fn assemble_frame(mut request: FrameRequest) -> Result<AssembledFrame, String> {
    let source = Source::new(&request.lines, &request.reading_order)?;
    for group in &mut request.groups {
        source.membership(&mut group.line_ids, &mut group.spans)?;
        group.id = anchor(&group.spans[0]);
    }
    request.groups.sort_by_key(|group| (source.positions[group.spans[0].line_id.as_str()], group.spans[0].start));
    for annotation in &mut request.annotations {
        source.membership(&mut annotation.line_ids, &mut annotation.spans)?;
    }
    let owners = documents(&request.groups, &request.annotations, &source, &request.continuations)?;
    for (group, owner) in request.groups.iter_mut().zip(owners) {
        group.document = Some(owner);
    }
    let logical_blocks = logical_blocks(&request.groups, &request.annotations, &source, &request.continuations)?;
    Ok(AssembledFrame { groups: request.groups, annotations: request.annotations, logical_blocks })
}

/// Who owns each block: the document holding its note's references, else the innermost document
/// opened at or before it ("source" before the first).
fn documents(groups: &[FrameBlock], annotations: &[FrameAnnotation], source: &Source<'_>,
             continuations: &FrameLinks) -> Result<Vec<String>, String> {
    let docs = annotations.iter().filter(|a| a.kind == "document").map(|a| (a.id.as_str(), a)).collect::<HashMap<_, _>>();
    let reference = |id: &str| -> Result<String, String> {
        if docs.contains_key(id) { return Ok(id.to_owned()); }
        let at = source.canonical(id);
        let mut matches = docs.values().filter(|a| anchor(&a.spans[0]) == at);
        let found = matches.next().ok_or("Document reference names absent declaration")?;
        if matches.next().is_some() { return Err("Ambiguous document opening; use its annotation identity".into()); }
        Ok(found.id.clone())
    };
    let mut parents = HashMap::new();
    for (id, doc) in &docs {
        let parent = match doc.attributes.get("parent") {
            Some(Value::String(parent)) if !parent.is_empty() => Some(reference(parent)?),
            _ => None,
        };
        parents.insert(id.to_string(), parent);
    }
    let mut events = Vec::new();
    for id in docs.keys() {
        let (mut seen, mut current, mut depth) = (HashSet::new(), Some(id.to_string()), 0);
        while let Some(value) = current {
            if !seen.insert(value.clone()) { return Err("Document parent cycle".into()); }
            current = parents[&value].clone();
            depth += usize::from(current.is_some());
        }
        events.push((source.location(&anchor(&docs[id].spans[0]))?, depth, id.to_string()));
    }
    events.sort();
    let (mut stack, mut opened) = (Vec::<String>::new(), Vec::new());
    for (at, _, id) in events {
        if let Some(parent) = &parents[&id] {
            let position = stack.iter().position(|v| v == parent).ok_or("Constituent parent must enclose its start")?;
            stack.truncate(position + 1);
        } else if !stack.is_empty() {
            return Err("A source requires one root document".into());
        }
        stack.push(id);
        opened.push((at, stack.last().unwrap().clone()));
    }
    let owner = |at: (usize, usize)| opened.iter().rev().find(|(position, _)| *position <= at)
        .map_or_else(|| "source".to_owned(), |(_, id)| id.clone());
    // A note belongs to the document holding its references when they name one document, its
    // continued parts with it.
    let note_links = source.links(continuations.get("note"));
    let notes = annotations.iter().filter(|a| a.kind == "note").map(|a| (anchor(&a.spans[0]), a)).collect::<HashMap<_, _>>();
    let mut note_owners = Vec::<(Vec<SourceSpan>, String)>::new();
    for (first, note) in &notes {
        let documents = spans(note.attributes.get("references"))?.iter()
            .map(|r| source.location(&anchor(r)).map(&owner)).collect::<Result<HashSet<_>, _>>()?;
        if documents.len() != 1 { continue; }
        let document = documents.into_iter().next().unwrap();
        let (mut current, mut seen) = (first.clone(), HashSet::new());
        while let Some(part) = notes.get(&current) {
            if !seen.insert(current.clone()) || current != *first && !spans(part.attributes.get("references"))?.is_empty() { break; }
            note_owners.push((part.spans.clone(), document.clone()));
            let Some(next) = note_links.get(&current) else { break };
            current = next.clone();
        }
    }
    groups.iter().map(|group| Ok(note_owners.iter()
        .find(|(note, _)| group.spans.iter().all(|s| note.iter().any(|n| n.line_id == s.line_id && n.start <= s.start && s.end <= n.end)))
        .map_or_else(|| source.location(&group.id).map(owner), |(_, id)| Ok(id.clone()))?)).collect()
}

/// Blocks joined by their paragraph, heading and note continuations, each chain keyed by its first.
fn logical_blocks(groups: &[FrameBlock], annotations: &[FrameAnnotation], source: &Source<'_>,
                  continuations: &FrameLinks) -> Result<BTreeMap<String, Vec<String>>, String> {
    let blocks = groups.iter().map(|b| b.id.as_str()).collect::<HashSet<_>>();
    let mut links = BTreeMap::new();
    for kind in ["paragraph", "heading"] {
        links.extend(source.links(continuations.get(kind)));
    }
    // A note continuation joins the block holding the note part's last text to the one where the
    // next part begins.
    let parts = annotations.iter().filter(|a| a.kind == "note").map(|a| (anchor(&a.spans[0]), a)).collect::<HashMap<_, _>>();
    for (id, next) in source.links(continuations.get("note")) {
        let at = source.location(&id)?;
        let note = parts.get(&id).copied().or_else(|| parts.values().copied().find(|part| part.spans.iter().any(|s| source.holds(s, at))))
            .ok_or("Note continuation names absent fragment")?;
        let target = source.location(&next)?;
        let following = parts.get(&next).copied().unwrap_or(note);
        let first = following.spans.iter().find(|s| source.holds(s, target)).ok_or("Note continuation target names absent fragment")?;
        let last = if following.id == note.id {
            note.spans.iter().filter(|s| (source.positions[s.line_id.as_str()], s.end) <= target).last()
                .ok_or("Note continuation has no preceding body")?
        } else { note.spans.last().unwrap() };
        let block = |test: &dyn Fn(&SourceSpan) -> bool| groups.iter().find(|b| b.spans.iter().any(test)).map(|b| b.id.clone());
        let left = block(&|s| s.line_id == last.line_id && s.start < last.end && last.end <= s.end).ok_or("Note body has no containing block")?;
        let right = block(&|s| s.line_id == first.line_id && s.start <= first.start && first.start < s.end).ok_or("Note target has no containing block")?;
        links.insert(left, right);
    }
    let mut incoming = HashSet::new();
    for (id, next) in &links {
        if !blocks.contains(id.as_str()) || !blocks.contains(next.as_str()) || !incoming.insert(next.clone()) {
            return Err("Text continuation must connect distinct blocks".into());
        }
    }
    let mut result = BTreeMap::new();
    let mut emitted = HashSet::new();
    for block in groups.iter().filter(|block| !incoming.contains(&block.id)) {
        let mut fragments = vec![block.id.clone()];
        while let Some(next) = links.get(fragments.last().unwrap()) {
            if !emitted.insert(next.clone()) { return Err("Text continuation cycle".into()); }
            fragments.push(next.clone());
        }
        result.insert(block.id.clone(), fragments);
    }
    Ok(result)
}
