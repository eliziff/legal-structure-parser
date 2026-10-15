//! Literal source-span composition used by annotation producers. No classification runs here.
use crate::{assemble_structure_annotations, Derivation, DocumentStructure, NodeKind, PrimaryRole,
    ScalarRange, ScalarText, SourceSpan, StructureNode, StructureOwnership};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use unicode_normalization::UnicodeNormalization;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CompositionLine {
    pub id: String,
    pub text: String,
    pub page: usize,
    #[serde(flatten)]
    pub evidence: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CompositionBlock {
    #[serde(default)]
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub line_ids: Vec<String>,
    #[serde(default)]
    pub spans: Vec<SourceSpan>,
    #[serde(default)]
    pub attributes: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership: Option<StructureOwnership>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CompositionAnnotation {
    #[serde(default)]
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub line_ids: Vec<String>,
    #[serde(default)]
    pub spans: Vec<SourceSpan>,
    #[serde(default)]
    pub attributes: Map<String, Value>,
    #[serde(flatten)]
    pub evidence: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CompositionLink {
    pub next: String,
    pub separator: Option<String>,
    #[serde(flatten)]
    pub attributes: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompositionEdit {
    ReplaceBlock { block: CompositionBlock },
    UpsertAnnotation { annotation: CompositionAnnotation },
    RemoveAnnotation { id: String },
    Move { members: Vec<SourceSpan>, anchor: SourceSpan, placement: crate::corrections::SourcePlacement },
    Join { anchor: String, next: String, separator: String },
    Continue { anchor: String, next: String, kind: String, separator: Option<String> },
    Resume { anchor: String, document: String },
    ClearContinue { anchor: String },
    ClearResume { anchor: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionRequest {
    pub lines: Vec<CompositionLine>,
    pub groups: Vec<CompositionBlock>,
    pub annotations: Vec<CompositionAnnotation>,
    #[serde(default)]
    pub removable_nodes: Vec<CompositionAnnotation>,
    pub reading_order: Vec<String>,
    pub targets: Vec<usize>,
    pub visible_pages: Vec<usize>,
    #[serde(default)]
    pub metadata_refs: Vec<SourceSpan>,
    #[serde(default)]
    pub source_correction: bool,
    #[serde(default)]
    pub native_navigation: Vec<crate::NativeNavigationWitness>,
    #[serde(default)]
    pub structure_tags: Vec<crate::PdfStructureTagWitness>,
    #[serde(default)]
    pub through: Option<usize>,
    #[serde(default)]
    pub joins: BTreeMap<String, CompositionLink>,
    #[serde(default)]
    pub continuations: BTreeMap<String, BTreeMap<String, CompositionLink>>,
    #[serde(default)]
    pub resumes: BTreeMap<String, Value>,
    #[serde(default)]
    pub manuscript_edits: crate::ManuscriptEdits,
    pub edits: Vec<CompositionEdit>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ManuscriptSpan {
    #[serde(flatten)]
    pub span: SourceSpan,
    pub separator: String,
}

#[derive(Debug, Serialize)]
pub struct AppliedComposition {
    /// Surviving native facts, including unanchored records that cannot be removed by a source edit.
    pub removable_nodes: Vec<CompositionAnnotation>,
    pub lines: Vec<CompositionLine>,
    pub groups: Vec<CompositionBlock>,
    pub annotations: Vec<CompositionAnnotation>,
    pub reading_order: Vec<String>,
    pub manuscript_spans: Vec<ManuscriptSpan>,
    pub manuscript_edits: crate::ManuscriptEdits,
    pub logical_blocks: BTreeMap<String, Vec<String>>,
    pub joins: BTreeMap<String, CompositionLink>,
    pub continuations: BTreeMap<String, BTreeMap<String, CompositionLink>>,
    pub resumes: BTreeMap<String, Value>,
    pub levels: BTreeMap<String, usize>,
    pub extents: BTreeMap<String, Vec<ScalarRange>>,
}

/// Give producer stream labels stable, source-backed first-member identities.
pub(crate) fn canonicalize_reading_streams(groups: &mut [CompositionBlock]) {
    let mut anchors = HashMap::new();
    for group in groups.iter() {
        if let Some(stream) = group.attributes.get("reading_stream_id").and_then(Value::as_str) {
            anchors.entry(stream.to_owned()).or_insert_with(|| group.id.clone());
        }
    }
    for group in groups {
        let value = group.attributes.get("reading_stream_id").and_then(Value::as_str)
            .and_then(|stream| anchors.get(stream)).cloned();
        group.attributes.insert("reading_stream_id".into(), json!(value));
        if let Some(owner) = &mut group.ownership { owner.reading_stream_id = value; }
    }
}

fn anchor(span: &SourceSpan) -> String { format!("{}@{}", span.line_id, span.start) }
fn overlaps(a: &SourceSpan, b: &SourceSpan) -> bool {
    a.line_id == b.line_id && a.start < b.end && b.start < a.end
}
fn contained(span: &SourceSpan, membership: &[SourceSpan]) -> bool {
    membership.iter().any(|s| s.line_id == span.line_id && s.start <= span.start && span.end <= s.end)
}
fn role(kind: &str) -> Result<PrimaryRole, String> {
    serde_json::from_value(json!(kind)).map_err(|_| format!("Unknown primary role: {kind}"))
}
fn source_spans(value: Option<&Value>) -> Result<Vec<SourceSpan>, String> {
    serde_json::from_value(value.cloned().unwrap_or_else(|| json!([]))).map_err(|e| e.to_string())
}
fn nullable_reference(attrs: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match attrs.get(key) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(id)) if !id.is_empty() => Ok(Some(id.clone())),
        _ => Err(format!("{key} requires an explicit source identity or null")),
    }
}
fn source_union(spans: &[SourceSpan]) -> BTreeMap<String, Vec<(usize, usize)>> {
    let mut result: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    for s in spans { result.entry(s.line_id.clone()).or_default().push((s.start, s.end)); }
    for ranges in result.values_mut() {
        ranges.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for &(start, end) in ranges.iter() {
            if let Some(last) = merged.last_mut().filter(|last| start <= last.1) { last.1 = last.1.max(end); }
            else { merged.push((start, end)); }
        }
        *ranges = merged;
    }
    result
}

pub(crate) struct Source<'a> {
    pub(crate) lines: HashMap<&'a str, &'a CompositionLine>,
    pub(crate) positions: HashMap<&'a str, usize>,
    pub(crate) targets: HashSet<usize>,
    pub(crate) visible: HashSet<usize>,
    pub(crate) metadata: &'a [SourceSpan],
    pub(crate) navigation: &'a [crate::NativeNavigationWitness],
}
impl Source<'_> {
    pub(crate) fn check(&self, spans: &[SourceSpan], target: bool) -> Result<(), String> {
        for span in spans {
            let line = self.lines.get(span.line_id.as_str()).ok_or("Span names absent source")?;
            if span.start >= span.end || span.end > line.text.chars().count() {
                return Err("Span outside source Unicode scalar boundaries".into());
            }
            if target && !self.targets.contains(&line.page) { return Err("Correction outside target pages".into()); }
        }
        Ok(())
    }
    fn metadata(&self, spans: &[SourceSpan]) -> Result<(), String> {
        self.check(spans, false)?;
        if spans.iter().any(|s| !self.visible.contains(&self.lines[s.line_id.as_str()].page) && !self.metadata.contains(s)) {
            return Err("New metadata references require attached-page evidence".into());
        }
        Ok(())
    }
    pub(crate) fn membership(&self, ids: &mut Vec<String>, spans: &mut Vec<SourceSpan>) -> Result<(), String> {
        if spans.is_empty() {
            *spans = ids.iter().map(|id| {
                self.lines.get(id.as_str()).map(|line| SourceSpan {line_id:id.clone(), start:0, end:line.text.chars().count()})
                    .ok_or_else(|| "Membership names absent source".to_owned())
            }).collect::<Result<_, _>>()?;
        }
        if spans.is_empty() { return Err("An object needs source membership".into()); }
        self.check(spans, false)?;
        spans.sort_by_key(|s| (self.positions[s.line_id.as_str()], s.start, s.end));
        if spans.windows(2).any(|s| overlaps(&s[0], &s[1])) { return Err("Object repeats source characters".into()); }
        let mut seen = HashSet::new();
        *ids = spans.iter().filter_map(|s| seen.insert(s.line_id.clone()).then_some(s.line_id.clone())).collect();
        Ok(())
    }
}

fn validate_annotation(annotation: &mut CompositionAnnotation, source: &Source<'_>, changed: bool) -> Result<(), String> {
    source.membership(&mut annotation.line_ids, &mut annotation.spans)?;
    if changed { source.check(&annotation.spans, true)?; }
    let attrs = &mut annotation.attributes;
    let read = |key: &str| source_spans(attrs.get(key));
    match annotation.kind.as_str() {
        "field" => {
            if !matches!(attrs.get("type").and_then(Value::as_str), Some("text"|"signature"|"checkbox"|"choice")) { return Err("Unknown field type".into()); }
            if !attrs.get("state").is_none_or(|v| v.is_null() || matches!(v.as_str(), Some("checked"|"unchecked")))
                || attrs.get("type").and_then(Value::as_str) != Some("checkbox") && attrs.get("state").is_some_and(|v| !v.is_null()) {
                return Err("Invalid field state".into());
            }
            for key in ["label", "value", "placeholder"] {
                let refs = read(key)?; source.check(&refs, false)?;
                if refs.iter().any(|s| !contained(s, &annotation.spans)) { return Err("Field member outside its extent".into()); }
            }
        }
        "note" => {
            if !matches!(attrs.get("kind").and_then(Value::as_str), Some("footnote"|"endnote"|"sidenote"|"table_note"|"author_note")) { return Err("Unknown note kind".into()); }
            let labels = read("label")?; source.check(&labels, false)?;
            if labels.iter().any(|s| !contained(s, &annotation.spans)) { return Err("Note label outside note".into()); }
            let references = read("references")?;
            if changed { source.metadata(&references)?; } else { source.check(&references, false)?; }
            let normalize=|spans:&[SourceSpan]| {
                let text=spans.iter().map(|s|ScalarText::new(&source.lines[s.line_id.as_str()].text).slice(s.start..s.end).unwrap().to_owned()).collect::<String>();
                text.nfkc().collect::<String>().trim_matches(|c|" .()[]{}:".contains(c)).to_owned()
            };
            let label=normalize(&labels);
            if references.iter().any(|r|label.is_empty()||normalize(std::slice::from_ref(r))!=label) {return Err("Note reference does not name its printed label".into());}
        }
        "navigation" => {
            let title = read("title")?;
            source.check(&title, false)?;
            let target = read("target")?;
            if changed { source.metadata(&target)?; } else { source.check(&target, false)?; }
            let parent = nullable_reference(attrs, "parent")?;
            let witness = nullable_reference(attrs, "witness_id")?;
            match attrs.get("kind").and_then(Value::as_str) {
                Some("contents") if witness.is_none() && (!changed || !title.is_empty())
                    && title.iter().all(|span| contained(span, &annotation.spans)) => {}
                Some("bookmark") if title.is_empty() && parent.is_none() && !target.is_empty()
                    && source_union(&target) == source_union(&annotation.spans) => {
                    let witness = witness.and_then(|id| source.navigation.iter().find(|entry| entry.id == id))
                        .ok_or("Bookmark confirmation names no native source witness")?;
                    if target.iter().any(|span| source.lines[span.line_id.as_str()].page != witness.page_index + 1) {
                        return Err("Bookmark target differs from native destination page".into());
                    }
                }
                _ => return Err("Navigation needs printed contents evidence or an existing native bookmark".into()),
            }
        }
        "quotation" => {
            if !matches!(attrs.get("layout").and_then(Value::as_str), Some("inline"|"display"))
                && (changed || !attrs.get("layout").is_none_or(Value::is_null)) { return Err("Unknown quotation layout".into()); }
            let refs = read("attribution")?;
            if changed { source.metadata(&refs)?; } else { source.check(&refs, false)?; }
        }
        "document" => {
            if attrs.get("kind").and_then(Value::as_str).is_none_or(|s| s.trim().is_empty())
                && (changed || !attrs.get("kind").is_none_or(Value::is_null)) { return Err("Document requires a kind".into()); }
            nullable_reference(attrs, "parent")?;
            if !attrs.get("relationship").is_none_or(|v| v.is_null() || matches!(v.as_str(), Some("component"|"attachment"|"exhibit"|"schedule"|"continuation"))) { return Err("Unknown document relationship".into()); }
            let mut refs = read("title")?;
            let empty_facets=Map::new();
            let facets = match attrs.get("facets") {Some(value)=>value.as_object().ok_or("Document facets must be an object")?,None=>&empty_facets};
            for (key, value) in facets {
                if !matches!(key.as_str(), "court"|"parties"|"author"|"date"|"procedural_role") { return Err("Unknown document facet".into()); }
                refs.extend(source_spans(Some(value))?);
            }
            if changed { source.metadata(&refs)?; } else { source.check(&refs, false)?; }
        }
        "table" => {
            let rows: Vec<Vec<Option<Vec<SourceSpan>>>> = serde_json::from_value(attrs.get("rows").cloned().ok_or("Table requires rows")?).map_err(|e|e.to_string())?;
            let width = rows.first().map(Vec::len).filter(|n| *n > 0).ok_or("Table requires populated rectangular rows")?;
            if rows.iter().any(|r| r.len()!=width) { return Err("Incomplete rectangular table".into()); }
            let merges: Vec<[usize;4]> = serde_json::from_value(attrs.get("merges").cloned().unwrap_or_else(||json!([]))).map_err(|e|e.to_string())?;
            let headers: Vec<usize> = serde_json::from_value(attrs.get("header_rows").cloned().ok_or("Table requires header_rows")?).map_err(|e|e.to_string())?;
            if headers.iter().any(|r| *r>=rows.len()) { return Err("Table header outside grid".into()); }
            let mut covered = HashSet::new();
            for [r,c,h,w] in merges {
                if h==0 || w==0 || r.checked_add(h).is_none_or(|v|v>rows.len()) || c.checked_add(w).is_none_or(|v|v>width) || rows[r][c].is_none() { return Err("Merge outside table grid".into()); }
                for rr in r..r+h { for cc in c..c+w {
                    if !covered.insert((rr,cc)) || (rr,cc)!=(r,c) && rows[rr][cc].is_some() { return Err("Overlapping or populated covered cell".into()); }
                }}
            }
            let mut spans = Vec::new();
            for (r,row) in rows.iter().enumerate() { for (c,cell) in row.iter().enumerate() {
                if let Some(cell) = cell { source.check(cell, changed)?; spans.extend(cell.clone()); }
                else if !covered.contains(&(r,c)) { return Err("Covered cell has no merge".into()); }
            }}
            if spans.is_empty() || spans.iter().enumerate().any(|(i,s)|spans[..i].iter().any(|p|overlaps(p,s))) { return Err("Empty table or repeated table text".into()); }
            if annotation.spans.first().map(anchor) != spans.first().map(anchor) { return Err("Table anchor differs from first populated cell".into()); }
            annotation.spans=spans;
            source.membership(&mut annotation.line_ids,&mut annotation.spans)?;
        }
        _ => return Err("Unknown structural annotation family".into()),
    }
    if annotation.id.is_empty() {
        annotation.id = if annotation.kind == "navigation" && annotation.attributes.get("kind").and_then(Value::as_str) == Some("bookmark") {
            format!("navigation:bookmark:{}", annotation.attributes["witness_id"].as_str().unwrap())
        } else if annotation.kind == "document" {
            return Err("Document declaration requires an explicit identity".into())
        } else { format!("{}:{}", annotation.kind, anchor(&annotation.spans[0])) };
    }
    Ok(())
}

/// Validate the canonical line-local scalar triples without changing printed text.
pub(crate) fn validate_manuscript_edits(
    edits: &crate::ManuscriptEdits,
    lengths: &HashMap<&str, usize>,
) -> Result<(), String> {
    for (id, entries) in edits {
        let length = lengths.get(id.as_str()).ok_or("Manuscript edit names absent source")?;
        let mut previous_end = 0;
        for (index, (start, end, _)) in entries.iter().enumerate() {
            if start > end || end > length || index > 0 && (*start < previous_end || *start <= entries[index - 1].0) {
                return Err("Manuscript edits must be ordered, nonoverlapping source scalar ranges".into());
            }
            previous_end = *end;
        }
    }
    Ok(())
}

pub(crate) fn validate_manuscript_boundaries<'a>(
    edits: &crate::ManuscriptEdits,
    spans: impl IntoIterator<Item = &'a SourceSpan>,
) -> Result<(), String> {
    for span in spans {
        if edits.get(&span.line_id).is_some_and(|entries| entries.iter().any(|(start, end, _)|
            [span.start, span.end].iter().any(|point| start < point && point < end))) {
            return Err("Manuscript boundary crosses character normalization".into());
        }
    }
    Ok(())
}

pub fn apply_structure_composition(mut request: CompositionRequest) -> Result<AppliedComposition, String> {
    let mut edits=Vec::new();
    for edit in std::mem::take(&mut request.edits) {
        if let CompositionEdit::Move {members,anchor,placement}=edit {
            for span in members.iter().chain(std::iter::once(&anchor)) {
                let line=request.lines.iter().find(|line|line.id==span.line_id).ok_or("Move names absent source")?;
                if span.start!=0 || span.end!=line.text.chars().count() {return Err("Move requires whole physical source fragments; split the source before moving partial membership".into());}
                if !request.targets.contains(&line.page) || !request.visible_pages.contains(&line.page) {return Err("Move outside target-page evidence".into());}
            }
            request.reading_order=crate::corrections::move_source_units(crate::corrections::SourceOrderRequest {
                order:request.reading_order,moves:vec![crate::corrections::SourceMove {
                    members:members.into_iter().map(|span|span.line_id).collect(),anchor:anchor.line_id,placement,
                }],
            })?;
        } else {edits.push(edit);}
    }
    request.edits=edits;
    let source = Source {
        lines: request.lines.iter().map(|l|(l.id.as_str(),l)).collect(),
        positions: request.reading_order.iter().enumerate().map(|(n,id)|(id.as_str(),n)).collect(),
        targets: request.targets.iter().copied().collect(), visible:request.visible_pages.iter().copied().collect(), metadata:&request.metadata_refs, navigation:&request.native_navigation,
    };
    if source.lines.len()!=request.lines.len() || source.positions.len()!=request.reading_order.len()
        || source.lines.len()!=source.positions.len() || source.lines.keys().any(|id|!source.positions.contains_key(id)) {
        return Err("Source identities and reading order must match exactly".into());
    }
    validate_manuscript_edits(&request.manuscript_edits, &source.lines.iter().map(|(id,line)|(*id,line.text.chars().count())).collect())?;
    if !source.targets.is_subset(&source.visible) { return Err("Target page evidence missing".into()); }
    let mut aliases=HashMap::new();
    for group in &mut request.groups {
        source.membership(&mut group.line_ids,&mut group.spans)?;
        let id=anchor(&group.spans[0]);
        if !group.id.is_empty() { aliases.insert(group.id.clone(),id.clone()); }
        if group.spans[0].start==0 { aliases.insert(group.spans[0].line_id.clone(),id.clone()); }
        group.id=id;
        group.attributes.remove("level");
    }
    request.groups.sort_by_key(|group|(source.positions[group.spans[0].line_id.as_str()],group.spans[0].start));
    canonicalize_reading_streams(&mut request.groups);
    for group in &mut request.groups {
        if let Some(Value::String(id))=group.attributes.get_mut("parent") {
            if let Some(canonical)=aliases.get(id) { *id=canonical.clone(); }
        }
    }
    for annotation in &mut request.annotations {
        source.membership(&mut annotation.line_ids,&mut annotation.spans)?;
        if !request.source_correction && request.through.is_none_or(|end|source.lines[annotation.spans[0].line_id.as_str()].page<=end) {
            validate_annotation(annotation,&source,false)?;
        }
    }
    let mut native_ids=HashSet::new();
    for annotation in &mut request.removable_nodes {
        if annotation.id.is_empty() || !native_ids.insert(annotation.id.clone()) {return Err("Native inventory needs unique identities".into());}
        if !annotation.line_ids.is_empty() || !annotation.spans.is_empty() {
            source.membership(&mut annotation.line_ids,&mut annotation.spans)?;
        }
        if annotation.evidence.get("parent_id").is_some_and(|value|!value.is_null()&&!value.is_string()) {
            return Err("Native parent identity must be a string or null".into());
        }
    }
    let mut replacements=Vec::new(); let mut changes=Vec::new(); let mut removals=HashSet::new();
    for edit in request.edits {
        match edit {
            CompositionEdit::ReplaceBlock {mut block} => {
                source.membership(&mut block.line_ids,&mut block.spans)?; source.check(&block.spans,true)?;
                if block.kind!="unknown" || !request.source_correction { role(&block.kind)?; }
                if source.lines[block.spans[0].line_id.as_str()].page != source.lines[block.spans.last().unwrap().line_id.as_str()].page { return Err("A block fragment must belong to one physical page".into()); }
                block.id=anchor(&block.spans[0]); block.attributes.remove("level");
                if block.kind!="unknown" {
                    let stream=nullable_reference(&block.attributes,"reading_stream_id")?;
                    if let Some(id)=stream { location(&id,&source)?; }
                    let parent=nullable_reference(&block.attributes,"parent")?;
                    if let Some(id)=parent { block.attributes.insert("parent".into(),json!(aliases.get(&id).unwrap_or(&id))); }
                }
                if let Some(kind)=block.attributes.get("locator_kind") {
                    if !matches!(kind.as_str(),Some("paragraph"|"section"|"list_item")) {return Err("Unknown locator kind".into());}
                    let marker=source_spans(block.attributes.get("marker"))?; source.check(&marker,true)?;
                    if marker.iter().any(|s|!contained(s,&block.spans)) || marker.is_empty() && kind.as_str()!=Some("list_item") {return Err("Printed locator marker outside block".into());}
                }
                replacements.push(block);
            }
            CompositionEdit::UpsertAnnotation {mut annotation} => {validate_annotation(&mut annotation,&source,true)?;changes.push(annotation);}
            CompositionEdit::Move {..} => unreachable!("moves are applied before constructing source order"),
            CompositionEdit::Join {anchor,next,separator} => {
                check_link_edit(&anchor,Some(&next),false,&source)?;
                if !manuscript_separator(&separator) {return Err("Invalid manuscript join separator".into());}
                let key=canonical_ref(&anchor,&source);
                request.joins.retain(|id,_|canonical_ref(id,&source)!=key);
                request.joins.insert(key,CompositionLink{next:canonical_ref(&next,&source),separator:Some(separator),attributes:BTreeMap::new()});
            }
            CompositionEdit::Continue {anchor,next,kind,separator} => {
                check_link_edit(&anchor,Some(&next),true,&source)?;
                if !matches!(kind.as_str(),"paragraph"|"heading"|"note"|"table"|"quotation"|"list") {return Err("Invalid continuation kind".into());}
                let text=matches!(kind.as_str(),"paragraph"|"heading"|"note");
                if text&&!separator.as_deref().is_some_and(manuscript_separator)||!text&&separator.is_some() {return Err("Authored continuation requires an explicit separator matching its kind".into());}
                let key=canonical_ref(&anchor,&source);let layer=request.continuations.entry(kind.clone()).or_default();
                layer.retain(|id,_|canonical_ref(id,&source)!=key);
                layer.insert(key,CompositionLink{next:canonical_ref(&next,&source),separator,attributes:BTreeMap::from([("kind".into(),json!(kind))])});
            }
            CompositionEdit::Resume {anchor,document} => {
                check_link_edit(&anchor,None,false,&source)?;
                let key=canonical_ref(&anchor,&source);
                request.resumes.retain(|id,_|canonical_ref(id,&source)!=key);
                request.resumes.insert(key,json!({"document":document}));
            }
            CompositionEdit::ClearContinue {anchor} => {
                let key=canonical_ref(&anchor,&source);location(&anchor,&source)?;
                let mut allowed=check_link_edit(&anchor,None,false,&source).is_ok();
                for layer in request.continuations.values() {
                    for (id,link) in layer {if canonical_ref(id,&source)==key && check_link_edit(&anchor,Some(&link.next),true,&source).is_ok(){allowed=true;}}
                }
                if !allowed {return Err("Continuation removal outside target pages".into());}
                for layer in request.continuations.values_mut(){layer.retain(|id,_|canonical_ref(id,&source)!=key);}
            }
            CompositionEdit::ClearResume {anchor} => {
                check_link_edit(&anchor,None,false,&source)?;let key=canonical_ref(&anchor,&source);
                request.resumes.retain(|id,_|canonical_ref(id,&source)!=key);
            }
            CompositionEdit::RemoveAnnotation {id} => {
                let a=request.annotations.iter().chain(request.removable_nodes.iter()).find(|a|a.id==id).ok_or("Removal names absent annotation")?;
                if native_ids.contains(&id) && a.kind=="page" {return Err("Physical page removal requires a source operation".into());}
                if !a.spans.iter().any(|s|source.targets.contains(&source.lines[s.line_id.as_str()].page)) {return Err("Removal outside target pages".into());}
                if !removals.insert(id) {return Err("Repeated annotation removal".into());}
            }
        }
    }
    let replaced:Vec<_>=replacements.iter().flat_map(|b|b.spans.clone()).collect();
    let affected:Vec<_>=request.groups.iter().filter(|b|b.spans.iter().any(|s|replaced.iter().any(|r|overlaps(s,r)))).flat_map(|b|b.spans.clone()).collect();
    if source_union(&replaced)!=source_union(&affected) {return Err("Replacement must account for all superseded block membership".into());}
    request.groups.retain(|b|!b.spans.iter().any(|s|replaced.iter().any(|r|overlaps(s,r))));
    request.groups.extend(replacements);
    request.groups.sort_by_key(|b|(source.positions[b.spans[0].line_id.as_str()],b.spans[0].start));
    let mut coverage:BTreeMap<&str,Vec<(usize,usize)>>=BTreeMap::new();
    let mut ids=HashSet::new();
    for group in &request.groups {
        if !ids.insert(group.id.clone()) {return Err("Duplicate span block identity".into());}
        for span in &group.spans {coverage.entry(span.line_id.as_str()).or_default().push((span.start,span.end));}
    }
    for line in &request.lines {
        let ranges=coverage.get_mut(line.id.as_str()).ok_or("Block coverage omitted source")?;ranges.sort_unstable();
        let mut cursor=0;
        for &(start,end) in ranges.iter() {if start!=cursor {return Err("Block membership overlaps or omits source characters".into());}cursor=end;}
        if cursor!=line.text.chars().count() {return Err("Block coverage omits source suffix".into());}
    }
    let key=|a:&CompositionAnnotation|(a.kind.clone(),
        if a.kind=="document" {a.id.clone()} else if a.kind=="navigation" && a.attributes.get("kind").and_then(Value::as_str)==Some("bookmark") {
            format!("bookmark:{}",a.attributes["witness_id"].as_str().unwrap())
        } else {anchor(&a.spans[0])});
    let changed_keys:HashSet<_>=changes.iter().map(key).collect();
    if changed_keys.len()!=changes.len() {return Err("Repeated annotation correction".into());}
    // Native descendants depend on their recorded parent object. Remove that
    // carried subtree literally instead of orphaning rows/cells or guessing from labels.
    loop {
        let descendants:Vec<_>=request.removable_nodes.iter().filter(|node|!removals.contains(&node.id)
            && node.evidence.get("parent_id").and_then(Value::as_str).is_some_and(|parent|native_ids.contains(parent)&&removals.contains(parent)))
            .map(|node|node.id.clone()).collect();
        if descendants.is_empty(){break;}
        removals.extend(descendants);
    }
    request.removable_nodes.retain(|node|!removals.contains(&node.id));
    request.annotations.retain(|a|!removals.contains(&a.id)&&!changed_keys.contains(&key(a)));
    request.annotations.extend(changes);
    if request.annotations.iter().map(|a|&a.id).collect::<HashSet<_>>().len()!=request.annotations.len() {return Err("Duplicate annotation identity".into());}
    if !request.source_correction {
        let entries: HashMap<_, _> = request.annotations.iter().filter(|a| a.kind == "navigation" && a.attributes.get("kind").and_then(Value::as_str)==Some("contents"))
            .map(|a| (anchor(&a.spans[0]), a)).collect();
        for entry in request.annotations.iter().filter(|a| a.kind == "navigation"
            && request.through.is_none_or(|end|source.lines[a.spans[0].line_id.as_str()].page<=end)) {
            let id=anchor(&entry.spans[0]);
            if let Some(parent) = nullable_reference(&entry.attributes, "parent")? {
                let parent = canonical_ref(&parent, &source);
                if !entries.contains_key(&parent) || location(&parent, &source)? >= location(&id, &source)? {
                    return Err("Contents parent must name an earlier printed contents entry".into());
                }
            }
            let target = source_spans(entry.attributes.get("target"))?;
            if !target.is_empty() && target.iter().all(|span|request.through.is_none_or(|end|source.lines[span.line_id.as_str()].page<=end)) {
                let inside = |members: &[SourceSpan]| {
                    let members = source_union(members);
                    target.iter().all(|span| members.get(&span.line_id).is_some_and(|ranges|
                        ranges.iter().any(|&(start, end)| start <= span.start && span.end <= end)))
                };
                let mut body = request.groups.iter().any(|group| role(&group.kind).is_ok_and(crate::annotations::navigation_body_role)
                    && inside(&group.spans));
                for document in request.annotations.iter().filter(|annotation| annotation.kind == "document") {
                    let title = source_spans(document.attributes.get("title"))?;
                    body |= inside(&title);
                }
                let contents = entries.values().flat_map(|entry| &entry.spans)
                    .any(|source| target.iter().any(|span| overlaps(source, span)));
                let excluded = request.groups.iter().filter(|group| role(&group.kind).is_ok_and(|role| !crate::annotations::navigation_body_role(role)))
                    .flat_map(|group| &group.spans).any(|source| target.iter().any(|span| overlaps(source, span)));
                if !body || contents || excluded {
                    return Err("Confirmed navigation target must name body membership or a document title outside printed contents, furniture and page labels".into());
                }
            }
        }
    }
    validate_manuscript_boundaries(&request.manuscript_edits, request.groups.iter().flat_map(|group|&group.spans))?;
    let (levels,extents)=if request.source_correction {(BTreeMap::new(),BTreeMap::new())}
        else { derive(&mut request.groups,&mut request.annotations,&source,&request.reading_order,&mut request.resumes,&request.continuations,request.through)? };
    if !request.source_correction { validate_links(&request.groups,&request.annotations,&source,&request.continuations,request.through)?; }
    let (manuscript_spans,logical_blocks)=if request.source_correction || request.through.is_some() {(Vec::new(),BTreeMap::new())} else {manuscript(&request.groups,&request.annotations,&source,&request.joins,&request.continuations)?};
    validate_manuscript_boundaries(&request.manuscript_edits, manuscript_spans.iter().map(|member|&member.span))?;
    Ok(AppliedComposition {removable_nodes:request.removable_nodes,lines:request.lines,groups:request.groups,annotations:request.annotations,
        reading_order:request.reading_order,manuscript_spans,manuscript_edits:request.manuscript_edits,logical_blocks,levels,extents,
        joins:request.joins,continuations:request.continuations,resumes:request.resumes})
}

fn check_link_edit(anchor:&str,next:Option<&str>,incoming:bool,source:&Source<'_>)->Result<(),String> {
    let anchor=canonical_ref(anchor,source);location(&anchor,source)?;
    let line=anchor.rsplit_once('@').map(|(line,_)|line).unwrap_or(anchor.as_str());
    let page=source.lines.get(line).ok_or("Relationship source absent")?.page;
    let carried = |id: &str| -> bool {
        let Some((line,offset))=id.rsplit_once('@') else {return false;};
        let Ok(offset)=offset.parse::<usize>() else {return false;};
        source.metadata.iter().any(|span|span.line_id==line && span.start==offset)
    };
    if !source.visible.contains(&page) && !carried(&anchor){return Err("Relationship source needs attached-page or inherited anchored evidence".into());}
    let target_page=if let Some(next)=next {
        let next=canonical_ref(next,source);location(&next,source)?;let line=next.rsplit_once('@').map(|(line,_)|line).unwrap_or(next.as_str());
        let page=source.lines.get(line).ok_or("Relationship target absent")?.page;
        if !source.visible.contains(&page) && !carried(&next){return Err("Relationship target needs attached-page or inherited anchored evidence".into());}
        Some(page)
    }else{None};
    if !source.targets.contains(&page) && !(incoming && target_page.is_some_and(|page|source.targets.contains(&page))) {return Err("Relationship outside target pages".into());}
    Ok(())
}

pub(crate) fn canonical_ref(id: &str, source: &Source<'_>) -> String {
    if source.lines.contains_key(id) { format!("{id}@0") } else { id.to_owned() }
}
pub(crate) fn location(id: &str, source: &Source<'_>) -> Result<(usize,usize),String> {
    if let Some(position)=source.positions.get(id) {return Ok((*position,0));}
    let (line,offset)=id.rsplit_once('@').ok_or("Relationship needs a source-span anchor")?;
    let offset=offset.parse::<usize>().map_err(|_|"Invalid source-span anchor")?;
    let text=source.lines.get(line).ok_or("Relationship names absent source")?;
    if offset>=text.text.chars().count() {return Err("Relationship anchor outside source".into());}
    Ok((source.positions[line],offset))
}
fn link_map(layer: Option<&BTreeMap<String,CompositionLink>>, source:&Source<'_>) -> BTreeMap<String,CompositionLink> {
    layer.into_iter().flat_map(|layer|layer.iter()).map(|(id,link)|(canonical_ref(id,source),CompositionLink{
        next:canonical_ref(&link.next,source),separator:link.separator.clone(),attributes:link.attributes.clone()})).collect()
}
fn derive(groups:&mut [CompositionBlock], annotations:&mut [CompositionAnnotation], source:&Source<'_>,
          order:&[String], resumes:&mut BTreeMap<String,Value>, continuations:&BTreeMap<String,BTreeMap<String,CompositionLink>>,through:Option<usize>)
    ->Result<(BTreeMap<String,usize>,BTreeMap<String,Vec<ScalarRange>>),String> {
    let mut text=String::new();let mut offsets=HashMap::new();let mut cursor=0;
    for id in order {let line=&source.lines[id.as_str()].text;offsets.insert(id.as_str(),cursor);text.push_str(line);text.push('\n');cursor+=line.encode_utf16().count()+1;}
    let global=|s:&SourceSpan| {let t=ScalarText::new(&source.lines[s.line_id.as_str()].text);ScalarRange{
        start:offsets[s.line_id.as_str()]+t.utf16(s.start),end:offsets[s.line_id.as_str()]+t.utf16(s.end)}};
    let docs:HashMap<_,_>=annotations.iter().filter(|a|a.kind=="document"&&through.is_none_or(|end|source.lines[a.spans[0].line_id.as_str()].page<=end)).map(|a|(a.id.clone(),a)).collect();
    let document_ref=|id:&str|->Result<String,String>{
        if docs.contains_key(id) { return Ok(id.to_owned()); }
        let at=canonical_ref(id,source);
        let mut matches=docs.values().filter(|a|anchor(&a.spans[0])==at);
        let found=matches.next().ok_or("Document reference names absent declaration")?;
        if matches.next().is_some() {return Err("Ambiguous document opening; use its annotation identity".into());}
        Ok(found.id.clone())
    };
    let mut parents=HashMap::new();let mut depths=HashMap::new();
    for (id,doc) in &docs {
        parents.insert(id.clone(),nullable_reference(&doc.attributes,"parent")?.map(|parent|document_ref(&parent)).transpose()?);
    }
    for id in docs.keys() {
        let mut seen=HashSet::new();let mut current=Some(id.as_str());let mut depth=0;
        while let Some(value)=current {
            if !seen.insert(value) {return Err("Document parent cycle".into());}
            current=parents[value].as_deref();if current.is_some(){depth+=1;}
        }
        depths.insert(id.clone(),depth);
    }
    let mut events:Vec<((usize,usize),bool,usize,String)>=docs.iter().map(|(id,doc)|Ok((location(&anchor(&doc.spans[0]),source)?,false,depths[id],id.clone()))).collect::<Result<_,String>>()?;
    for (id,resume) in resumes.iter_mut() {
        if through.is_some_and(|end|{let line=id.rsplit_once('@').map(|(id,_)|id).unwrap_or(id);source.lines.get(line).is_some_and(|l|l.page>end)}){continue;}
        let document=resume.get("document").and_then(Value::as_str).ok_or("Resumption requires a document identity")?;
        let document=document_ref(document)?;
        resume["document"]=json!(document);
        events.push((location(id,source)?,true,depths[&document],document));
    }
    events.sort_by(|left,right|(left.0,left.1,left.2,&left.3).cmp(&(right.0,right.1,right.2,&right.3)));
    let mut stack:Vec<String>=Vec::new();let mut ownership:Vec<((usize,usize),String)>=Vec::new();
    for (at,resume,_,id) in events {
        if resume {
            let position=stack.iter().position(|v|v==&id).ok_or("Resumption must name an enclosing document")?;stack.truncate(position+1);
        } else {
            if let Some(parent)=&parents[&id] {
                let position=stack.iter().position(|v|v==parent).ok_or("Constituent parent must enclose its start")?;stack.truncate(position+1);
            } else if !stack.is_empty() {return Err("A source requires one root document; use resumption for returning content".into());}
            stack.push(id.clone());
        }
        ownership.push((at,stack.last().unwrap().clone()));
    }
    let owner=|at:(usize,usize)|ownership.iter().rev().find(|(position,_)|*position<=at).map(|(_,id)|id.clone()).unwrap_or_else(||"source".into());
    let quotes:HashMap<_,_>=annotations.iter().filter(|a|a.kind=="quotation").map(|a|(anchor(&a.spans[0]),a)).collect();
    let quote_links=link_map(continuations.get("quotation"),source);
    let mut logical_quotes=Vec::new();let mut consumed=HashSet::new();let mut quotation_identity=HashMap::new();
    for (first,quote) in &quotes {
        if quote_links.values().any(|l|&l.next==first) {continue;}
        let mut current=first.clone();let mut spans=Vec::new();
        loop {
            if !consumed.insert(current.clone()) {return Err("Quotation continuation cycle or repeated fragment".into());}
            let part=quotes.get(&current).ok_or("Quotation continuation names absent fragment")?;
            quotation_identity.insert(part.id.clone(),quote.id.clone());spans.extend(part.spans.clone());
            if let Some(link)=quote_links.get(&current) {
                if !quotes.contains_key(&link.next) {
                    let target=location(&link.next,source)?;
                    if part.spans.iter().any(|s|source.positions[s.line_id.as_str()]==target.0&&s.start<=target.1&&target.1<s.end){break;}
                }
                current=link.next.clone();
            } else {break;}
        }
        logical_quotes.push((quote.id.clone(),spans));
    }
    if consumed.len()!=quotes.len() {return Err("Quotation continuation cycle".into());}
    // A note belongs to the document holding its references when they name one document; a note
    // printed after the next document's opening (a footnote area) otherwise falls to position.
    let mut note_owners:Vec<(Vec<SourceSpan>,String)>=Vec::new();
    let note_links=link_map(continuations.get("note"),source);
    let notes:HashMap<_,_>=annotations.iter().filter(|a|a.kind=="note").map(|a|(anchor(&a.spans[0]),&*a)).collect();
    for (first,note) in &notes {
        let references=source_spans(note.attributes.get("references"))?;
        let documents:HashSet<_>=references.iter().map(|r|location(&anchor(r),source).map(owner)).collect::<Result<_,_>>()?;
        if documents.len()!=1 {continue;}
        let document=documents.into_iter().next().unwrap();
        let mut current=first.clone();let mut seen=HashSet::new();
        while let Some(part)=notes.get(&current) {
            if !seen.insert(current.clone()) || current!=*first && !source_spans(part.attributes.get("references"))?.is_empty() {break;}
            note_owners.push((part.spans.clone(),document.clone()));
            let Some(link)=note_links.get(&current) else {break;};
            current=link.next.clone();
        }
    }
    drop(notes);
    let note_owner=|spans:&[SourceSpan]|note_owners.iter().find(|(note,_)|spans.iter().all(|s|note.iter().any(|n|n.line_id==s.line_id&&n.start<=s.start&&s.end<=n.end))).map(|(_,id)|id.clone());
    let observed:HashSet<_>=groups.iter().filter(|g|through.is_none_or(|end|source.lines[g.spans[0].line_id.as_str()].page<=end)).map(|g|g.id.clone()).collect();
    let all_ids:HashSet<_>=groups.iter().map(|g|g.id.clone()).collect();
    let mut nodes=Vec::new();
    for group in groups.iter_mut() {
        if !observed.contains(&group.id){continue;}
        let parent=nullable_reference(&group.attributes,"parent")?.map(|id|canonical_ref(&id,source));
        group.attributes.insert("parent".into(),json!(parent));
        let at=location(&group.id,source)?;let document_id=owner(at);
        for span in &group.spans {
            if owner((source.positions[span.line_id.as_str()],span.start))!=document_id || owner((source.positions[span.line_id.as_str()],span.end-1))!=document_id {return Err("Block crosses constituent ownership".into());}
        }
        let document_id=note_owner(&group.spans).unwrap_or(document_id);
        let markers=source_spans(group.attributes.get("marker"))?;
        let scope_spans=if markers.is_empty(){&group.spans}else{&markers};
        let quotation_id=crate::composition_annotations::quotation_scope(scope_spans,&logical_quotes,None).map(str::to_owned);
        let reading_stream_id=group.attributes.get("reading_stream_id").and_then(Value::as_str).map(str::to_owned);
        group.ownership=Some(StructureOwnership{document_id,quotation_id,reading_stream_id});
        let ranges=group.spans.iter().map(global).collect::<Vec<_>>();
        let range=ScalarRange{start:ranges.iter().map(|r|r.start).min().unwrap(),end:ranges.iter().map(|r|r.end).max().unwrap()};
        let primary=role(&group.kind)?;
        if parent.as_ref().is_some_and(|id|!all_ids.contains(id)){return Err("Block parent names absent membership".into());}
        let derived_parent=parent.filter(|id|observed.contains(id));
        let mut node=StructureNode::new(group.id.clone(),if primary==PrimaryRole::Heading{NodeKind::Heading}else{NodeKind::Prose},range,"source",Derivation::Model,derived_parent);
        node.role=Some(primary);node.ownership=group.ownership.clone();node.ranges=ranges;
        if primary==PrimaryRole::Heading&&node.ranges.len()==1 {node.heading_range=Some(node.range);}
        node.line_ids=group.line_ids.clone();nodes.push(node);
    }
    let stream_roots: HashMap<_,_> = groups.iter().filter(|group|observed.contains(&group.id))
        .map(|group|(group.id.as_str(),group)).collect();
    let mut first_members = HashMap::new();
    for group in groups.iter().filter(|group|observed.contains(&group.id)) {
        let Some(scope)=&group.ownership else {continue;};
        let Some(stream)=scope.reading_stream_id.as_deref() else {continue;};
        let root=stream_roots.get(stream).ok_or("Reading stream names absent source block")?;
        let root_scope=root.ownership.as_ref().ok_or("Reading stream root has no ownership")?;
        if root_scope.document_id!=scope.document_id || root_scope.reading_stream_id.as_deref()!=Some(stream) {
            return Err("Reading stream must name its first member in the same document".into());
        }
        let first=first_members.entry(stream).or_insert(group.id.as_str());
        if *first!=stream {return Err("Reading stream identity must be its first source member".into());}
    }
    for annotation in annotations.iter_mut() {
        if annotation.kind=="document" && parents.contains_key(&annotation.id) {annotation.attributes.insert("parent".into(),json!(parents[&annotation.id]));}
        let document_id=if annotation.kind=="document" {annotation.id.clone()}
            else if let Some(document)=(annotation.kind=="note").then(||note_owner(&annotation.spans)).flatten() {document}
            else {owner(location(&anchor(&annotation.spans[0]),source)?)};
        // Exclude the canonical self, but retain each fragment's enclosing scope
        // so continuation validation can detect crossings of an outer quotation.
        let logical_self=quotation_identity.get(&annotation.id);
        let quotation_id=crate::composition_annotations::quotation_scope(&annotation.spans,&logical_quotes,logical_self.map(String::as_str)).map(str::to_owned);
        let streams:HashSet<_>=groups.iter().filter(|g|g.spans.iter().any(|s|annotation.spans.iter().any(|r|overlaps(s,r))))
            .map(|g|g.ownership.as_ref().and_then(|o|o.reading_stream_id.clone())).collect();
        let reading_stream_id=if streams.len()==1 {streams.into_iter().next().flatten()}else{None};
        annotation.evidence.insert("ownership".into(),serde_json::to_value(StructureOwnership{document_id,quotation_id,reading_stream_id}).map_err(|e|e.to_string())?);
    }
    let graph:DocumentStructure=serde_json::from_value(json!({"schema_version":"legalpdf.document-structure.v1","document_id":"source","offset_unit":"utf16","provider":"annotations","revision":"","text":text,"text_sha256":"","scope":{"kind":"complete"},"origins":[],"nodes":nodes,"diagnostics":[]})).map_err(|e|e.to_string())?;
    let derived=assemble_structure_annotations(graph)?;
    Ok((derived.levels,derived.extents))
}

fn manuscript_separator(separator: &str) -> bool { separator.chars().all(char::is_whitespace) }

fn manuscript(groups:&[CompositionBlock],annotations:&[CompositionAnnotation],source:&Source<'_>,
              joins:&BTreeMap<String,CompositionLink>,continuations:&BTreeMap<String,BTreeMap<String,CompositionLink>>)
    ->Result<(Vec<ManuscriptSpan>,BTreeMap<String,Vec<String>>),String> {
    let blocks:HashMap<_,_>=groups.iter().map(|b|(b.id.clone(),b)).collect();
    let mut links=BTreeMap::new();
    for kind in ["paragraph","heading"] {
        for (id,link) in link_map(continuations.get(kind),source) {
            if links.insert(id,link).is_some(){return Err("Multiple text continuations for one block".into());}
        }
    }
    let note_parts:HashMap<_,_>=annotations.iter().filter(|a|a.kind=="note").map(|a|(anchor(&a.spans[0]),a)).collect();
    for (id,link) in link_map(continuations.get("note"),source) {
        let at=location(&id,source)?;
        let note=note_parts.get(&id).copied().or_else(||note_parts.values().copied().find(|part|part.spans.iter().any(|s|source.positions[s.line_id.as_str()]==at.0&&s.start<=at.1&&at.1<s.end)))
            .ok_or("Note continuation names absent fragment")?;
        let target=location(&link.next,source)?;
        let next=note_parts.get(&link.next).copied().unwrap_or(note);
        let first=next.spans.iter().find(|s|source.positions[s.line_id.as_str()]==target.0&&s.start<=target.1&&target.1<s.end)
            .ok_or("Note continuation target names absent fragment")?;
        let last=if next.id==note.id {note.spans.iter().filter(|s|(source.positions[s.line_id.as_str()],s.end)<=target).last().ok_or("Note continuation has no preceding body")?}
            else {note.spans.last().unwrap()};
        let left=groups.iter().find(|b|b.spans.iter().any(|s|s.line_id==last.line_id&&s.start<last.end&&last.end<=s.end)).ok_or("Note body has no containing block")?;
        let right=groups.iter().find(|b|b.spans.iter().any(|s|s.line_id==first.line_id&&s.start<=first.start&&first.start<s.end)).ok_or("Note target has no containing block")?;
        let link=CompositionLink{next:right.id.clone(),separator:link.separator,attributes:link.attributes};
        if links.get(&left.id).is_some_and(|old:&CompositionLink|old.next!=link.next||old.separator!=link.separator) {return Err("Conflicting note and paragraph continuations".into());}
        links.insert(left.id.clone(),link);
    }
    let mut incoming=HashSet::new();
    for (id,link) in &links {
        if !blocks.contains_key(id)||!blocks.contains_key(&link.next)||!incoming.insert(link.next.clone())||location(id,source)? >= location(&link.next,source)? {return Err("Text continuation must connect distinct ordered blocks".into());}
        if !link.separator.as_deref().is_some_and(manuscript_separator) {return Err("Text continuation requires an explicit separator".into());}
        if blocks[id].ownership!=blocks[&link.next].ownership {return Err("Continuation crosses document or quotation scope".into());}
    }
    let joins=link_map(Some(joins),source);
    let mut result=Vec::new();let mut logical_blocks=BTreeMap::new();let mut emitted=HashSet::new();let mut used_joins=HashSet::new();
    for block in groups {
        if incoming.contains(&block.id){continue;}
        let mut fragments=Vec::new();
        let mut current=block.id.clone();let mut leading=if result.is_empty(){""}else{"\n\n"}.to_owned();
        loop {
            if !emitted.insert(current.clone()){return Err("Text continuation cycle".into());}
            fragments.push(current.clone());
            let part=blocks[&current];
            for (n,span) in part.spans.iter().enumerate() {
                let separator=if n==0 {leading.clone()} else {
                    let previous=&part.spans[n-1];let key=anchor(previous);
                    let linked=joins.get_key_value(&key).or_else(||joins.get_key_value(&format!("{}@0",previous.line_id)));
                    if let Some((join_key,link))=linked {
                        if link.next!=anchor(span)&&link.next!=format!("{}@0",span.line_id){return Err("Word join target is not its adjacent member".into());}
                        used_joins.insert(join_key.clone());link.separator.clone().filter(|s|manuscript_separator(s)).ok_or("Invalid word separator")?
                    } else if previous.line_id==span.line_id&&previous.end==span.start {"".into()} else {
                        let left=source.lines[previous.line_id.as_str()];let right=source.lines[span.line_id.as_str()];
                        let same=left.evidence.get("source_id").is_some_and(|id|!id.is_null()&&Some(id)==right.evidence.get("source_id"));
                        let adjacent=left.evidence.get("source_range").and_then(Value::as_array).and_then(|v|v.get(1))
                            .zip(right.evidence.get("source_range").and_then(Value::as_array).and_then(|v|v.first())).is_some_and(|(a,b)|a==b);
                        if same&&adjacent {"".into()}else{" ".into()}
                    }
                };
                result.push(ManuscriptSpan{span:span.clone(),separator});
            }
            if let Some(link)=links.get(&current) {
                // The object link names fragment starts; a word join names the
                // actual adjacent source members at their shared text seam.
                let previous=part.spans.last().unwrap();
                let next=blocks[&link.next].spans.first().unwrap();
                if let Some((join_key,join))=joins.get_key_value(&anchor(previous)) {
                    if join.next!=anchor(next) || join.separator!=link.separator {
                        return Err("Word join conflicts with its object continuation seam".into());
                    }
                    used_joins.insert(join_key.clone());
                }
                current=link.next.clone();leading=link.separator.clone().unwrap();
            } else {break;}
        }
        logical_blocks.insert(block.id.clone(),fragments);
    }
    let logical_ids: HashMap<_, _> = logical_blocks.iter().flat_map(|(first, fragments)|
        fragments.iter().map(move |fragment| (fragment.as_str(), first.as_str()))).collect();
    for fragments in logical_blocks.values() {
        let role = &blocks[&fragments[0]].kind;
        let mut expected = None;
        for fragment in fragments {
            if &blocks[fragment].kind != role {
                return Err("Continued block fragments have different primary roles".into());
            }
            let parent = nullable_reference(&blocks[fragment].attributes, "parent")?
                .map(|parent| logical_ids.get(parent.as_str()).copied().unwrap_or(parent.as_str()).to_owned());
            if expected.as_ref().is_some_and(|previous| previous != &parent) {
                return Err("Continued block fragments have different explicit parents".into());
            }
            expected = Some(parent);
        }
    }
    if emitted.len()!=groups.len(){return Err("Text continuations omitted a block".into());}
    if joins.keys().any(|key|!used_joins.contains(key)){return Err("Word join does not connect adjacent members of one block".into());}
    Ok((result,logical_blocks))
}

fn validate_links(groups:&[CompositionBlock],annotations:&[CompositionAnnotation],source:&Source<'_>,
                  continuations:&BTreeMap<String,BTreeMap<String,CompositionLink>>,through:Option<usize>)->Result<(),String>{
    let blocks:HashMap<_,_>=groups.iter().map(|g|(g.id.as_str(),g)).collect();
    let observed=|id:&str|->Result<bool,String>{let at=location(id,source)?;Ok(through.is_none_or(|end|source.lines.values().any(|l|source.positions[l.id.as_str()]==at.0&&l.page<=end)))};
    let object=|kind:&str,id:&str|->Result<&CompositionAnnotation,String>{
        if let Some(found)=annotations.iter().find(|a|a.kind==kind&&anchor(&a.spans[0])==id){return Ok(found);}
        let at=location(id,source)?;
        let mut matches=annotations.iter().filter(|a|a.kind==kind&&a.spans.iter().any(|s|source.positions[s.line_id.as_str()]==at.0&&s.start<=at.1&&at.1<s.end));
        let found=matches.next().ok_or("Continuation names absent annotation fragment")?;
        if matches.next().is_some(){return Err("Continuation anchor names ambiguous annotation fragments".into());}
        Ok(found)
    };
    for (kind,layer) in continuations {
        if !matches!(kind.as_str(),"paragraph"|"heading"|"note"|"table"|"quotation"|"list"){return Err("Unknown continuation kind".into());}
        let mut incoming=HashSet::new();
        for (first,link) in link_map(Some(layer),source){
            if !observed(&first)?{continue;}
            if location(&first,source)? >= location(&link.next,source)?||!incoming.insert(link.next.clone()){return Err("Continuation is backwards or branches into an existing fragment".into());}
            let text=matches!(kind.as_str(),"paragraph"|"heading"|"note");
            let pending=text && link.separator.is_none() && through.is_some() && !observed(&link.next)?;
            if text&&!pending&&!link.separator.as_deref().is_some_and(manuscript_separator)||!text&&link.separator.is_some(){return Err("Text continuation needs a literal separator when its target is authored".into());}
            let target_observed=observed(&link.next)?;
            let mut endpoint_owners=Vec::new();
            for id in std::iter::once(first.as_str()).chain(target_observed.then_some(link.next.as_str())){
                if matches!(kind.as_str(),"paragraph"|"heading"|"list"){
                    let block=blocks.get(id).ok_or("Continuation must name an exact block start")?;
                    if kind=="heading"&&block.kind!="heading"||kind!="heading"&&matches!(block.kind.as_str(),"heading"|"furniture"|"page_label")
                        ||kind=="list"&&block.attributes.get("locator_kind").and_then(Value::as_str)!=Some("list_item") {
                        return Err("Continuation kind differs from block role".into());
                    }
                    endpoint_owners.push(serde_json::to_value(&block.ownership).map_err(|e|e.to_string())?);
                } else {
                    endpoint_owners.push(object(kind,id)?.evidence.get("ownership").cloned().ok_or("Continuation fragment has no ownership")?);
                }
            }
            if endpoint_owners.len()==2&&endpoint_owners[0]!=endpoint_owners[1]{return Err("Continuation crosses document, quotation or reading-stream ownership".into());}
            if target_observed&&matches!(kind.as_str(),"table"|"note"){
                let a=object(kind,&first)?;let b=object(kind,&link.next)?;
                if kind=="table" {
                    let width=|a:&CompositionAnnotation|a.attributes.get("rows").and_then(Value::as_array).and_then(|v|v.first()).and_then(Value::as_array).map(Vec::len);
                    if width(a)!=width(b){return Err("Continued table column counts differ".into());}
                } else if a.attributes.get("kind")!=b.attributes.get("kind") {
                    return Err("Continued note kinds differ".into());
                }
            }
        }
    }
    let owner=|span:&SourceSpan|groups.iter().find(|g|g.spans.iter().any(|s|s.line_id==span.line_id&&s.start<=span.start&&span.start<s.end)).and_then(|g|g.ownership.as_ref());
    for annotation in annotations.iter().filter(|a|a.kind=="note"){
        let Some(body)=owner(&annotation.spans[0])else{continue;};
        for reference in source_spans(annotation.attributes.get("references"))?{
            if let Some(reference)=owner(&reference){if reference.document_id!=body.document_id{return Err("Note reference crosses constituent ownership".into());}}
        }
        if groups.iter().any(|g|g.kind=="instruction"&&g.spans.iter().any(|s|annotation.spans.iter().any(|r|overlaps(s,r)))){return Err("Printed instruction is classified as a note".into());}
    }
    Ok(())
}
