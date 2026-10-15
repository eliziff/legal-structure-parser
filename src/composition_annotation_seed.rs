//! Seed editable annotation facts from typed structure and retained producer metadata.
use crate::{CompositionAnnotation, CompositionLine, DocumentStructure, NodeKind, ScalarRange,
    ScalarText, SourceSpan, StructureScopeKind};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

type Result<T> = std::result::Result<T, String>;

/// Annotation pseudo nodes belong to the composition view, not typed structural nodes.
pub(crate) fn typed_structure(raw: &Value) -> Result<DocumentStructure> {
    let mut typed = raw.clone();
    if let Some(nodes) = typed.get_mut("nodes").and_then(Value::as_array_mut) {
        nodes.retain(|node| !matches!(node["kind"].as_str(), Some("document"|"quotation"|"note")));
    }
    serde_json::from_value(typed).map_err(|error| error.to_string())
}

struct Source<'a> {
    text: &'a ScalarText<'a>,
    lines: &'a [(&'a CompositionLine, ScalarRange, usize)],
}
impl Source<'_> {
    fn spans(&self, ranges: &[ScalarRange]) -> Result<Vec<SourceSpan>> {
        let mut result = Vec::new();
        for range in ranges {
            for (line, extent, scalar_start) in self.lines {
                let start = range.start.max(extent.start);
                let end = range.end.min(extent.end);
                if start < end {
                    result.push(SourceSpan {
                        line_id: line.id.clone(),
                        start: self.text.scalar_at_utf16(start).ok_or("Annotation splits a source character")? - scalar_start,
                        end: self.text.scalar_at_utf16(end).ok_or("Annotation splits a source character")? - scalar_start,
                    });
                }
            }
        }
        Ok(result)
    }
    fn restore(&self, value: &Value) -> Result<Value> {
        match value {
            Value::Object(object) if object.contains_key("line_id") && object.contains_key("range") => {
                let range = serde_json::from_value(object["range"].clone()).map_err(|e|e.to_string())?;
                Ok(json!(self.spans(&[range])?))
            }
            Value::Object(object) => object.iter().map(|(key,value)|
                Ok((key.clone(),self.restore(value)?))).collect::<Result<Map<_,_>>>().map(Value::Object),
            Value::Array(values) => {
                let mut restored = Vec::new();
                for value in values {
                    let item = self.restore(value)?;
                    if value.get("line_id").is_some() && value.get("range").is_some() {
                        restored.extend(item.as_array().unwrap().iter().cloned());
                    } else { restored.push(item); }
                }
                Ok(Value::Array(restored))
            }
            _ => Ok(value.clone()),
        }
    }
    fn raw_spans(&self, raw: &Value) -> Result<Vec<SourceSpan>> {
        if let Some(spans) = raw.get("spans") {
            return serde_json::from_value(self.restore(spans)?).map_err(|e|e.to_string());
        }
        if let Some(ranges) = raw.get("ranges").and_then(Value::as_array).filter(|v|!v.is_empty()) {
            return self.spans(&serde_json::from_value::<Vec<ScalarRange>>(Value::Array(ranges.clone())).map_err(|e|e.to_string())?);
        }
        if let Some(range) = raw.get("range") {
            return self.spans(&[serde_json::from_value(range.clone()).map_err(|e|e.to_string())?]);
        }
        let ids: HashSet<_> = raw.get("line_ids").and_then(Value::as_array).into_iter().flatten()
            .filter_map(Value::as_str).collect();
        Ok(self.lines.iter().filter(|(line,_,_)|ids.contains(line.id.as_str())).map(|(line,_,_)|
            SourceSpan {line_id:line.id.clone(),start:0,end:line.text.chars().count()}).collect())
    }
}

fn anchor(span: &SourceSpan) -> String { format!("{}@{}",span.line_id,span.start) }
fn node_ranges(node: &crate::StructureNode) -> &[ScalarRange] {
    if node.ranges.is_empty() {std::slice::from_ref(&node.range)} else {&node.ranges}
}
fn raw_by_id<'a>(raw: &'a Value, collection: &str, id: &str) -> Option<&'a Value> {
    raw.get(collection)?.as_array()?.iter().find(|value|value["id"].as_str()==Some(id))
}
fn put(structure: &DocumentStructure, annotations: &mut Vec<CompositionAnnotation>, id: &str, kind: &str,
    spans: Vec<SourceSpan>, attributes: Map<String,Value>, raw: Option<&Value>, ownership: Option<Value>) {
    if spans.is_empty() { return; }
    let mut seen = HashSet::new();
    let line_ids = spans.iter().filter_map(|span|seen.insert(span.line_id.clone()).then_some(span.line_id.clone())).collect();
    let mut evidence = annotations.iter().find(|annotation|annotation.id==id)
        .map(|annotation|annotation.evidence.clone()).unwrap_or_default();
    let retained: BTreeMap<_,_> = raw.and_then(Value::as_object).into_iter().flat_map(|object|object.iter())
        .filter(|(key,_)|!matches!(key.as_str(),"id"|"kind"|"line_ids"|"spans"|"attributes"|"ownership"|"native_witness"))
        .map(|(key,value)|(key.clone(),value.clone())).collect();
    let mut witnesses=evidence.remove("native_witness")
        .and_then(|value|value.as_array().cloned()).unwrap_or_default();
    for witness in raw.and_then(|value|value.get("native_witness"))
        .and_then(Value::as_array).into_iter().flatten() {
        if !witnesses.contains(witness) {witnesses.push(witness.clone());}
    }
    if !retained.is_empty() {
        let witness=json!({
            "document_id":structure.document_id,"text_sha256":structure.text_sha256,
            "source_sha256":structure.source_sha256,"offset_unit":structure.offset_unit,
            "facts":retained
        });
        if !witnesses.contains(&witness) {witnesses.push(witness);}
    }
    if !witnesses.is_empty() {evidence.insert("native_witness".into(),Value::Array(witnesses));}
    if let Some(ownership)=ownership.or_else(||raw.and_then(|value|value.get("ownership")).cloned()) {
        evidence.insert("ownership".into(),ownership);
    }
    let annotation = CompositionAnnotation {id:id.to_owned(),kind:kind.to_owned(),line_ids,spans,attributes,evidence};
    if let Some(previous)=annotations.iter_mut().find(|annotation|annotation.id==id) {*previous=annotation;}
    else {annotations.push(annotation);}
}
fn attributes(source: &Source<'_>, raw: Option<&Value>) -> Result<Map<String,Value>> {
    let Some(raw)=raw else {return Ok(Map::new());};
    if let Some(attrs)=raw.get("attributes").and_then(Value::as_object) {
        return source.restore(&Value::Object(attrs.clone()))?.as_object().cloned().ok_or("Invalid annotation attributes".into());
    }
    let mut attrs=raw.as_object().cloned().unwrap_or_default();
    for key in ["id","kind","line_ids","spans","range","ranges","page_indexes","proof","ownership","native_witness","label_range","label_ranges","body_range","body_ranges","value_ranges","placeholder_ranges","source_ranges","target_ranges","title_ranges","references"] {attrs.remove(key);}
    if let Some(references)=raw.get("references").and_then(Value::as_array) {
        if references.iter().all(|value|value.get("line_id").is_some()) {
            attrs.insert("references".into(),source.restore(&Value::Array(references.clone()))?);
        }
    }
    source.restore(&Value::Object(attrs))?.as_object().cloned().ok_or("Invalid annotation attributes".into())
}

pub(crate) fn seed_annotations(
    structure: &DocumentStructure, raw: &Value, text: &ScalarText<'_>,
    lines: &[(&CompositionLine, ScalarRange, usize)],
) -> Result<Vec<CompositionAnnotation>> {
    let source=Source {text,lines};
    let mut annotations=Vec::new();
    // Exported annotation metadata carries facts absent from the narrower typed
    // scope view, including document kind/facets and original producer evidence.
    for collection in ["nodes","annotations","documents"] {
        for value in raw.get(collection).and_then(Value::as_array).into_iter().flatten() {
            let kind=value.get("kind").and_then(Value::as_str).unwrap_or("");
            if !matches!(kind,"document"|"quotation"|"note"|"field"|"table"|"navigation")
                || collection=="nodes" && !matches!(kind,"document"|"quotation"|"note") {continue;}
            let id=value.get("id").and_then(Value::as_str).ok_or("Annotation lacks identity")?;
            let spans=source.raw_spans(value)?;
            put(structure,&mut annotations,id,kind,spans,attributes(&source,Some(value))?,Some(value),None);
        }
    }
    for field in &structure.fields {
        let existing=annotations.iter().find(|a|a.id==field.id).map(|a|a.attributes.clone()).unwrap_or_default();
        let mut attrs=existing;
        attrs.insert("type".into(),json!(field.kind));
        attrs.insert("state".into(),json!(field.state));
        for (key,ranges) in [("label",&field.label_ranges),("value",&field.value_ranges),("placeholder",&field.placeholder_ranges)] {
            attrs.insert(key.into(),json!(source.spans(ranges)?));
        }
        put(structure,&mut annotations,&field.id,"field",source.spans(&field.ranges)?,attrs,
            raw_by_id(raw,"fields",&field.id),field.ownership.as_ref().map(|owner|json!(owner)));
    }
    for note in &structure.notes {
        let mut ranges=note.label_ranges.iter().chain(&note.body_ranges).copied().collect();
        ScalarRange::normalize_union(&mut ranges);
        let mut attrs=annotations.iter().find(|a|a.id==note.id).map(|a|a.attributes.clone()).unwrap_or_default();
        attrs.insert("kind".into(),json!(note.kind));
        attrs.insert("label".into(),json!(source.spans(&note.label_ranges)?));
        attrs.insert("references".into(),json!(source.spans(&note.references.iter().map(|reference|reference.range).collect::<Vec<_>>())?));
        put(structure,&mut annotations,&note.id,"note",source.spans(&ranges)?,attrs,
            raw_by_id(raw,"notes",&note.id),note.ownership.as_ref().map(|owner|json!(owner)));
    }
    for table in structure.nodes.iter().filter(|node|node.kind==NodeKind::Table) {
        let row_nodes: Vec<_>=structure.nodes.iter().filter(|node|node.kind==NodeKind::Row && node.parent_id.as_deref()==Some(table.id.as_str())).collect();
        let row_ids: HashSet<_>=row_nodes.iter().map(|node|node.id.as_str()).collect();
        let cells: Vec<_>=structure.nodes.iter().filter(|node|node.kind==NodeKind::Cell
            && node.parent_id.as_deref().is_some_and(|id|row_ids.contains(id))).collect();
        // Incomplete native geometry remains native evidence, never an invented grid.
        if row_nodes.is_empty() || row_nodes.iter().any(|row|row.row.is_none())
            || cells.iter().any(|cell|cell.row.is_none() || cell.column.is_none()) {continue;}
        if row_nodes.iter().any(|row|row.row==Some(0)) || cells.iter().any(|cell|
            cell.row==Some(0)||cell.column==Some(0)||cell.row_span==Some(0)||cell.column_span==Some(0)) {
            return Err("Table coordinates must be one-based".into());
        }
        let table_raw=raw_by_id(raw,"nodes",&table.id);
        let mut height=row_nodes.iter().filter_map(|node|node.row).max().unwrap_or(0);
        let mut width=0;
        for cell in &cells {
            height=height.max(cell.row.unwrap().checked_add(cell.row_span.unwrap_or(1)-1).ok_or("Table row overflow")?);
            width=width.max(cell.column.unwrap().checked_add(cell.column_span.unwrap_or(1)-1).ok_or("Table column overflow")?);
        }
        height=height.max(table_raw.and_then(|value|value["row_count"].as_u64()).unwrap_or(0) as usize);
        width=width.max(table_raw.and_then(|value|value["column_count"].as_u64()).unwrap_or(0) as usize);
        if width==0 {continue;}
        let mut rows=vec![vec![Some(Vec::<SourceSpan>::new());width];height];
        let mut occupied=HashSet::new();
        let mut merges=Vec::new();
        for cell in &cells {
            let (r,c)=(cell.row.unwrap()-1,cell.column.unwrap()-1);
            let (h,w)=(cell.row_span.unwrap_or(1),cell.column_span.unwrap_or(1));
            for rr in r..r+h {for cc in c..c+w {
                if !occupied.insert((rr,cc)) {return Err("Native table cells overlap".into());}
                rows[rr][cc]=None;
            }}
            rows[r][c]=Some(source.spans(node_ranges(cell))?);
            if h>1||w>1 {merges.push([r,c,h,w]);}
        }
        let headers=if let Some(headers)=&table.header_rows {
            headers.iter().map(|row|row.checked_sub(1).ok_or_else(||"Table header must be one-based".to_owned())).collect::<Result<Vec<_>>>()?
        } else {
            row_nodes.iter().filter(|row| {
                let row_cells: Vec<_>=cells.iter().filter(|cell|cell.parent_id.as_deref()==Some(row.id.as_str())).collect();
                !row_cells.is_empty() && row_cells.iter().all(|cell|cell.markup_tag.as_deref()==Some("th"))
            }).map(|row|row.row.unwrap()-1).collect()
        };
        let spans=rows.iter().flatten().filter_map(Option::as_ref).flatten().cloned().collect();
        let mut attrs=annotations.iter().find(|a|a.id==table.id).map(|a|a.attributes.clone()).unwrap_or_default();
        attrs.insert("rows".into(),json!(rows));attrs.insert("header_rows".into(),json!(headers));attrs.insert("merges".into(),json!(merges));
        put(structure,&mut annotations,&table.id,"table",spans,attrs,table_raw,table.ownership.as_ref().map(|owner|json!(owner)));
    }
    let scopes: Vec<_>=structure.scope_annotations.iter().filter(|scope|
        matches!(scope.kind,StructureScopeKind::Document|StructureScopeKind::Quotation))
        .map(|scope|Ok((scope,source.spans(&scope.ranges)?))).collect::<Result<_>>()?;
    let document_ids: HashSet<_>=scopes.iter().filter(|(scope,spans)|
        scope.kind==StructureScopeKind::Document&&!spans.is_empty()).map(|(scope,_)|scope.id.as_str())
        .chain(annotations.iter().filter(|a|a.kind=="document").map(|a|a.id.as_str())).map(str::to_owned).collect();
    for (scope,spans) in scopes {
        if spans.is_empty() {continue;}
        let mut attrs=annotations.iter().find(|a|a.id==scope.id).map(|a|a.attributes.clone()).unwrap_or_default();
        let document=scope.kind==StructureScopeKind::Document;
        if document {
            if scope.parent_scope_id.as_ref().is_some_and(|parent|!document_ids.contains(parent)) {
                return Err("Document scope parent has no source membership".into());
            }
            attrs.entry("kind").or_insert(Value::Null);
            attrs.entry("facets").or_insert(json!({}));
            attrs.insert("parent".into(),json!(scope.parent_scope_id));
            attrs.insert("relationship".into(),json!(scope.relationship));
            attrs.insert("title".into(),json!(source.spans(&scope.title_ranges)?));
        } else {
            attrs.insert("layout".into(),json!(scope.layout));
            attrs.insert("attribution".into(),json!(source.spans(&scope.attribution_ranges)?));
        }
        let opening=if document {vec![spans[0].clone()]} else {spans};
        let prior=annotations.iter().find(|a|a.id==scope.id).map(|a|serde_json::to_value(a).unwrap());
        put(structure,&mut annotations,&scope.id,if document {"document"} else {"quotation"},opening,attrs,
            prior.as_ref(),Some(json!(scope.ownership)));
    }
    let mut navigation=Vec::new();let mut anchors=HashMap::new();
    for entry in &structure.navigation {
        let sources=source.spans(&entry.source_ranges)?;
        let targets=source.spans(&entry.target_ranges)?;
        match entry.kind.as_str() {
            "contents" if !sources.is_empty()=>{anchors.insert(entry.id.as_str(),anchor(&sources[0]));}
            "bookmark" if !targets.is_empty()=>{}
            _=>continue,
        }
        navigation.push((entry,sources,targets));
    }
    for (entry,sources,targets) in navigation {
        let bookmark=entry.kind=="bookmark";
        let parent=if bookmark {None} else {entry.parent_id.as_ref().map(|id|
            anchors.get(id.as_str()).cloned().ok_or("Contents parent has no source membership")).transpose()?};
        let attrs=json!({"kind":entry.kind,"title":source.spans(&entry.title_ranges)?,
            "target":targets,"parent":parent,"witness_id":entry.witness_id}).as_object().unwrap().clone();
        let spans=if bookmark {targets} else {sources};
        put(structure,&mut annotations,&entry.id,"navigation",spans,attrs,raw_by_id(raw,"navigation",&entry.id),None);
    }
    if !annotations.iter().any(|a|a.kind=="document") {
        if let Some((line,_,_))=lines.iter().find(|(line,_,_)|!line.text.is_empty()) {
            put(structure,&mut annotations,&structure.document_id,"document",
                vec![SourceSpan {line_id:line.id.clone(),start:0,end:line.text.chars().count()}],
                json!({"kind":structure.doc_type,"parent":null,"relationship":null,"title":[],"facets":{}}).as_object().unwrap().clone(),None,None);
        }
    }
    Ok(annotations)
}

/// Retained native facts carry editable membership without claiming semantic validation.
pub(crate) fn seed_removable_nodes(
    structure: &DocumentStructure, raw: &Value, retained_ids: &[String],
    text: &ScalarText<'_>, lines: &[(&CompositionLine, ScalarRange, usize)],
) -> Result<Vec<CompositionAnnotation>> {
    let source=Source {text,lines};
    let retained: HashSet<_>=retained_ids.iter().map(String::as_str).collect();
    raw.get("nodes").and_then(Value::as_array).into_iter().flatten()
        .filter(|node|node.get("id").and_then(Value::as_str).is_some_and(|id|retained.contains(id)))
        .map(|node| {
            // Line identity by itself is not exact membership. Preserve an
            // unanchored fact instead of claiming all text on its source lines.
            let mut anchored=node.clone();
            anchored.as_object_mut().ok_or("Native node must be an object")?.remove("line_ids");
            let spans=source.raw_spans(&anchored)?;
            let mut seen=HashSet::new();
            let line_ids=spans.iter().filter_map(|span|seen.insert(span.line_id.clone())
                .then_some(span.line_id.clone())).collect();
            let mut facts=node.clone();
            facts.as_object_mut().unwrap().remove("native_witness");
            let mut witnesses=node.get("native_witness").and_then(Value::as_array).cloned().unwrap_or_default();
            let witness=json!({
                "document_id":structure.document_id,"text_sha256":structure.text_sha256,
                "source_sha256":structure.source_sha256,"offset_unit":structure.offset_unit,
                "facts":facts
            });
            if !witnesses.contains(&witness) {witnesses.push(witness);}
            let evidence=BTreeMap::from([
                ("parent_id".into(),node.get("parent_id").cloned().unwrap_or(Value::Null)),
                ("native_witness".into(),Value::Array(witnesses))
            ]);
            Ok(CompositionAnnotation {
                id:node["id"].as_str().ok_or("Native node lacks identity")?.to_owned(),
                kind:node["kind"].as_str().ok_or("Native node lacks kind")?.to_owned(),
                line_ids,spans,attributes:Map::new(),evidence,
            })
        }).collect()
}
