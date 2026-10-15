//! Apply supplied structural facts literally. Detection and model selection are producers.
use crate::{
    DocumentStructure, NodeKind, PrimaryRole, ScalarRange, ScalarText, StructureNode, StructureOwnership,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

/// Navigation identifies owned body text, never a page or structural grid envelope.
pub(crate) fn navigation_body_role(role: PrimaryRole) -> bool {
    !matches!(role, PrimaryRole::Furniture | PrimaryRole::PageLabel)
}

fn navigation_body_ranges(node: &StructureNode, document_scope: bool) -> Option<&[ScalarRange]> {
    if matches!(node.kind, NodeKind::Page | NodeKind::List | NodeKind::Table | NodeKind::Row)
        || node.role.is_some_and(|role| !navigation_body_role(role)) { return None; }
    if node.kind == NodeKind::Section && document_scope {
        return node.heading_range.as_ref().map(std::slice::from_ref);
    }
    if !node.ranges.is_empty() { return Some(&node.ranges); }
    if node.kind == NodeKind::Section { return node.heading_range.as_ref().map(std::slice::from_ref); }
    Some(std::slice::from_ref(&node.range))
}

fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum StructureAnnotationEdit {
    SetParent {
        node_id: String,
        #[serde(deserialize_with = "required_nullable")]
        parent_id: Option<String>,
    },
    SetRole {
        node_id: String,
        role: PrimaryRole,
    },
    SetOwnership {
        node_id: String,
        ownership: StructureOwnership,
    },
    SetLocator {
        node_id: String,
        #[serde(deserialize_with = "required_nullable")]
        locator_kind: Option<String>,
        #[serde(deserialize_with = "required_nullable")]
        label: Option<String>,
        #[serde(deserialize_with = "required_nullable")]
        marker_range: Option<ScalarRange>,
    },
    SetMembership {
        node_id: String,
        ranges: Vec<ScalarRange>,
    },
    SetHeading {
        node_id: String,
        #[serde(deserialize_with = "required_nullable")]
        range: Option<ScalarRange>,
    },
    ReplaceNodes {
        remove: Vec<String>,
        nodes: Vec<StructureNode>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructureAnnotationRequest {
    pub structure: DocumentStructure,
    pub expected_document_id: String,
    pub expected_revision: String,
    /// Every incoming character range is in Unicode scalar coordinates of source text.
    pub edits: Vec<StructureAnnotationEdit>,
}

#[derive(Debug, Serialize)]
pub struct AppliedStructureAnnotations {
    pub structure: DocumentStructure,
    pub levels: BTreeMap<String, usize>,
    /// Exact descendant membership in the structure's exported UTF-16 coordinates.
    pub extents: BTreeMap<String, Vec<ScalarRange>>,
}

fn exported_range(text: &ScalarText<'_>, range: ScalarRange) -> Result<ScalarRange, String> {
    if !range.valid(text.len()) {
        return Err("Annotation range is outside source text".into());
    }
    Ok(ScalarRange {
        start: text.utf16(range.start),
        end: text.utf16(range.end),
    })
}

fn envelope(ranges: &[ScalarRange]) -> Result<ScalarRange, String> {
    let start = ranges
        .iter()
        .map(|r| r.start)
        .min()
        .ok_or("Membership must contain a source span")?;
    Ok(ScalarRange {
        start,
        end: ranges.iter().map(|r| r.end).max().unwrap(),
    })
}

pub fn apply_structure_annotations(
    request: StructureAnnotationRequest,
) -> Result<AppliedStructureAnnotations, String> {
    let mut structure = request.structure;
    if structure.document_id != request.expected_document_id
        || structure.revision != request.expected_revision
    {
        return Err("Annotation source representation changed".into());
    }
    if structure.offset_unit != "utf16" {
        return Err("Expected UTF-16 document export".into());
    }
    let text = ScalarText::new(&structure.text);
    for edit in request.edits {
        if let StructureAnnotationEdit::ReplaceNodes { remove, mut nodes } = edit {
            if structure
                .rendered_text
                .as_deref()
                .is_some_and(|rendered| rendered != structure.text)
            {
                return Err(
                    "Replacement nodes need an explicit source-to-manuscript mapping".into(),
                );
            }
            let removed = remove.iter().collect::<HashSet<_>>();
            if removed.len() != remove.len()
                || removed
                    .iter()
                    .any(|id| !structure.nodes.iter().any(|n| &n.id == *id))
            {
                return Err("Replacement names an absent or repeated node".into());
            }
            for node in &mut nodes {
                node.range = exported_range(&text, node.range)?;
                for range in &mut node.ranges {
                    *range = exported_range(&text, *range)?;
                }
                node.heading_range = node
                    .heading_range
                    .map(|r| exported_range(&text, r))
                    .transpose()?;
                node.marker_range = node
                    .marker_range
                    .map(|r| exported_range(&text, r))
                    .transpose()?;
                node.content_start = node
                    .content_start
                    .map(|p| {
                        exported_range(&text, ScalarRange { start: p, end: p }).map(|r| r.start)
                    })
                    .transpose()?;
                if node.rendered_range.is_some()
                    || !node.rendered_ranges.is_empty()
                    || !node.rendered_membership.is_empty()
                {
                    return Err("Replacement nodes must use source membership, not supplied rendered offsets".into());
                }
            }
            structure.nodes.retain(|node| !removed.contains(&node.id));
            structure.nodes.extend(nodes);
            continue;
        }
        let id = match &edit {
            StructureAnnotationEdit::SetParent { node_id, .. }
            | StructureAnnotationEdit::SetRole { node_id, .. }
            | StructureAnnotationEdit::SetOwnership { node_id, .. }
            | StructureAnnotationEdit::SetLocator { node_id, .. }
            | StructureAnnotationEdit::SetMembership { node_id, .. }
            | StructureAnnotationEdit::SetHeading { node_id, .. } => node_id,
            StructureAnnotationEdit::ReplaceNodes { .. } => unreachable!(),
        };
        let node = structure
            .nodes
            .iter_mut()
            .find(|node| &node.id == id)
            .ok_or("Annotation names an absent node")?;
        match edit {
            StructureAnnotationEdit::SetParent { parent_id, .. } => node.parent_id = parent_id,
            StructureAnnotationEdit::SetRole { role, .. } => node.role = Some(role),
            StructureAnnotationEdit::SetOwnership { ownership, .. } => {
                node.ownership = Some(ownership)
            }
            StructureAnnotationEdit::SetLocator {
                locator_kind,
                label,
                marker_range,
                ..
            } => {
                node.locator_kind = locator_kind;
                node.label = label;
                node.marker_range = marker_range.map(|r| exported_range(&text, r)).transpose()?;
            }
            StructureAnnotationEdit::SetMembership { ranges, .. } => {
                if structure
                    .rendered_text
                    .as_deref()
                    .is_some_and(|rendered| rendered != structure.text)
                {
                    return Err(
                        "Membership edits need an explicit source-to-manuscript mapping".into(),
                    );
                }
                node.ranges = ranges
                    .into_iter()
                    .map(|r| exported_range(&text, r))
                    .collect::<Result<_, _>>()?;
                node.range = envelope(&node.ranges)?;
                node.rendered_range = None;
                node.rendered_ranges.clear();
                node.rendered_membership.clear();
            }
            StructureAnnotationEdit::SetHeading { range, .. } => {
                node.heading_range = range.map(|r| exported_range(&text, r)).transpose()?
            }
            StructureAnnotationEdit::ReplaceNodes { .. } => unreachable!(),
        }
    }
    assemble_structure_annotations(structure)
}

/// Derive views from recorded parents and membership, without a stack or inferred reparenting.
pub fn assemble_structure_annotations(
    mut structure: DocumentStructure,
) -> Result<AppliedStructureAnnotations, String> {
    let text = ScalarText::new(&structure.text);
    let mut source_ids = HashSet::new();
    for fragment in &structure.source_fragments {
        if fragment.id.is_empty()
            || !source_ids.insert(&fragment.id)
            || fragment.range.start > fragment.range.end
            || text
                .slice_utf16(fragment.range.start..fragment.range.end)
                .is_none()
        {
            return Err("Invalid source fragment identity or UTF-16 range".into());
        }
    }
    crate::composition::validate_manuscript_edits(&structure.manuscript_edits,
        &structure.source_fragments.iter().map(|fragment| (fragment.id.as_str(),
            text.slice_utf16(fragment.range.start..fragment.range.end).unwrap().chars().count())).collect())?;
    let valid_anchor = |offset: usize| text.slice_utf16(offset..offset).is_some()
        && structure.source_fragments.iter().any(|part| part.range.start <= offset && offset < part.range.end);
    let mut continuation_sources = HashSet::new();
    let mut continuation_targets = HashSet::new();
    for link in structure.continuations.as_deref().unwrap_or_default() {
        let kind = serde_json::to_string(&link.kind).map_err(|error| error.to_string())?;
        let text_link = matches!(link.kind, crate::ContinuationKind::Paragraph | crate::ContinuationKind::Heading | crate::ContinuationKind::Note);
        if link.from >= link.to || !valid_anchor(link.from) || !valid_anchor(link.to)
            || !continuation_sources.insert((kind.clone(), link.from))
            || !continuation_targets.insert((kind, link.to))
            || (text_link && !link.separator.as_ref().is_some_and(|value| value.chars().all(char::is_whitespace)))
            || (!text_link && link.separator.is_some()) {
            return Err("Invalid exact source continuation".into());
        }
    }
    let mut join_sources = HashSet::new();
    let mut join_targets = HashSet::new();
    for link in &structure.joins {
        if link.from >= link.to || !valid_anchor(link.from) || !valid_anchor(link.to)
            || !join_sources.insert(link.from) || !join_targets.insert(link.to)
            || !link.separator.chars().all(char::is_whitespace) {
            return Err("Invalid exact source join".into());
        }
    }
    for node in &structure.nodes {
        if let Some(headers) = &node.header_rows {
            if node.kind != NodeKind::Table || headers.iter().any(|row| *row == 0)
                || headers.windows(2).any(|rows| rows[0] >= rows[1])
                || headers.iter().any(|row| !structure.nodes.iter().any(|child|
                    child.kind == NodeKind::Row && child.parent_id.as_deref() == Some(node.id.as_str())
                        && child.row == Some(*row))) {
                return Err("Table headers must name distinct ordered one-based rows of their table".into());
            }
        }
    }
    let scopes = structure
        .scope_annotations
        .iter()
        .map(|scope| (scope.id.as_str(), scope))
        .collect::<HashMap<_, _>>();
    if scopes.len() != structure.scope_annotations.len() || scopes.keys().any(|id| id.is_empty()) {
        return Err("Scope identities are empty or duplicated".into());
    }
    for scope in &structure.scope_annotations {
        use crate::StructureScopeKind;
        if scope.ownership.document_id.is_empty() || scope.ranges.is_empty() {
            return Err("Scope lacks document identity or exact membership".into());
        }
        let identity = match scope.kind {
            StructureScopeKind::Document => Some(scope.ownership.document_id.as_str()),
            StructureScopeKind::Quotation => scope.ownership.quotation_id.as_deref(),
            StructureScopeKind::ReadingStream => scope.ownership.reading_stream_id.as_deref(),
        };
        if identity != Some(scope.id.as_str()) {
            return Err("Scope identity differs from its ownership".into());
        }
        for range in scope
            .ranges
            .iter()
            .chain(scope.owned_ranges.iter())
            .chain(scope.title_ranges.iter())
            .chain(scope.attribution_ranges.iter())
        {
            if range.start > range.end || text.slice_utf16(range.start..range.end).is_none() {
                return Err("Scope range outside source text".into());
            }
        }
        for title in scope.title_ranges.iter().chain(&scope.owned_ranges) {
            if !scope
                .ranges
                .iter()
                .any(|part| part.start <= title.start && title.end <= part.end)
            {
                return Err("Scope title outside membership".into());
            }
        }
        let mut seen = HashSet::from([scope.id.as_str()]);
        let mut parent = scope.parent_scope_id.as_deref();
        while let Some(id) = parent {
            if !seen.insert(id) {
                return Err("Scope containment cycle".into());
            }
            if id == structure.document_id {
                break;
            }
            parent = scopes
                .get(id)
                .ok_or("Scope parent is absent")?
                .parent_scope_id
                .as_deref();
        }
    }
    let mut document_members = structure.scope_annotations.iter()
        .filter(|scope| scope.kind == crate::StructureScopeKind::Document)
        .flat_map(|scope| scope.owned_ranges.iter().filter(|range| range.start < range.end).map(move |range| (range.start, range.end, scope.id.as_str())))
        .collect::<Vec<_>>();
    document_members.sort_by_key(|&(start, end, _)| (start, end));
    for (index, &(_, end, owner)) in document_members.iter().enumerate() {
        if document_members[index + 1..].iter().take_while(|&&(next_start, _, _)| next_start < end)
            .any(|&(_, _, next_owner)| owner != next_owner) {
            return Err("Document-owned source memberships overlap".into());
        }
    }
    // Older authoritative producers can omit this optional collection entirely.
    if !scopes.is_empty() {
        for owner in structure
            .nodes
            .iter()
            .filter_map(|node| node.ownership.as_ref())
            .chain(structure.notes.iter().filter_map(|note| note.ownership.as_ref()))
            .chain(structure.fields.iter().filter_map(|field| field.ownership.as_ref()))
        {
            if owner.document_id != structure.document_id
                && !scopes
                    .get(owner.document_id.as_str())
                    .is_some_and(|scope| scope.kind == crate::StructureScopeKind::Document)
            {
                return Err("Node document scope is absent".into());
            }
            for (id, kind) in [
                (
                    owner.quotation_id.as_deref(),
                    crate::StructureScopeKind::Quotation,
                ),
                (
                    owner.reading_stream_id.as_deref(),
                    crate::StructureScopeKind::ReadingStream,
                ),
            ] {
                if let Some(id) = id {
                    if !scopes.get(id).is_some_and(|scope| {
                        scope.kind == kind && scope.ownership.document_id == owner.document_id
                    }) {
                        return Err("Node scope is absent or belongs to another document".into());
                    }
                }
            }
        }
    }
    let mut field_ids = HashSet::new();
    for field in &structure.fields {
        if field.id.is_empty() || !field_ids.insert(&field.id) || field.ranges.is_empty() {
            return Err("Field identity or exact membership is missing or duplicated".into());
        }
        if field.kind != crate::FieldKind::Checkbox && field.state.is_some() {
            return Err("Only checkbox fields have a checked state".into());
        }
        if field.ownership.as_ref().is_some_and(|owner| owner.document_id.is_empty()) {
            return Err("Field document ownership is empty".into());
        }
        for range in field.ranges.iter().chain(&field.label_ranges)
            .chain(&field.value_ranges).chain(&field.placeholder_ranges) {
            if range.start >= range.end || text.slice_utf16(range.start..range.end).is_none() {
                return Err("Field span outside source text or empty".into());
            }
        }
        for range in field.label_ranges.iter().chain(&field.value_ranges).chain(&field.placeholder_ranges) {
            if !field.ranges.iter().any(|part| part.start <= range.start && range.end <= part.end) {
                return Err("Field member outside exact field membership".into());
            }
        }
    }
    let rendered = ScalarText::new(
        structure
            .rendered_text
            .as_deref()
            .unwrap_or(&structure.text),
    );
    let mut positions = HashMap::new();
    for (i, node) in structure.nodes.iter().enumerate() {
        if node.id.is_empty() || positions.insert(node.id.clone(), i).is_some() {
            return Err("Empty or duplicate structural identity".into());
        }
        for range in std::iter::once(&node.range)
            .chain(node.ranges.iter())
            .chain(node.heading_range.iter())
            .chain(node.marker_range.iter())
        {
            if range.start > range.end || text.slice_utf16(range.start..range.end).is_none() {
                return Err("Structural span outside source text".into());
            }
        }
        if node
            .ownership
            .as_ref()
            .is_some_and(|scope| scope.document_id.is_empty())
        {
            return Err("Structural document ownership is empty".into());
        }
        for range in node
            .rendered_range
            .iter()
            .chain(node.rendered_ranges.iter())
            .chain(node.rendered_membership.iter())
        {
            if range.start > range.end || rendered.slice_utf16(range.start..range.end).is_none() {
                return Err("Rendered span outside manuscript text".into());
            }
        }
        if !node.ranges.is_empty() && envelope(&node.ranges)? != node.range {
            return Err("Node envelope differs from exact membership".into());
        }
        let membership = if node.ranges.is_empty() {
            std::slice::from_ref(&node.range)
        } else {
            &node.ranges
        };
        for range in node.heading_range.iter().chain(node.marker_range.iter()) {
            if !membership
                .iter()
                .any(|part| part.start <= range.start && range.end <= part.end)
            {
                return Err(format!("Heading or marker lies outside node membership: {}", node.id).into());
            }
        }
    }
    let native_navigation: HashMap<_, _> = structure
        .native_navigation
        .iter()
        .map(|entry| (entry.id.as_str(), entry))
        .collect();
    if native_navigation.len() != structure.native_navigation.len()
        || native_navigation.keys().any(|id| id.is_empty())
    {
        return Err("Native navigation identities are empty or duplicated".into());
    }
    let navigation: HashMap<_, _> = structure
        .navigation
        .iter()
        .map(|entry| (entry.id.as_str(), entry))
        .collect();
    if navigation.len() != structure.navigation.len() || navigation.keys().any(|id| id.is_empty()) {
        return Err("Navigation identities are empty or duplicated".into());
    }
    for entry in &structure.native_navigation {
        let mut seen = HashSet::from([entry.id.as_str()]);
        let mut parent = entry.parent_id.as_deref();
        while let Some(id) = parent {
            if !seen.insert(id) {
                return Err("Native navigation hierarchy contains a cycle".into());
            }
            parent = native_navigation
                .get(id)
                .ok_or("Native navigation parent is absent")?
                .parent_id
                .as_deref();
        }
    }
    for entry in &structure.navigation {
        for range in entry
            .source_ranges
            .iter()
            .chain(&entry.target_ranges)
            .chain(&entry.title_ranges)
        {
            if range.start >= range.end || text.slice_utf16(range.start..range.end).is_none() {
                return Err("Navigation evidence lies outside source text".into());
            }
        }
        if entry.target_ranges.is_empty() != entry.target_line_ids.is_empty() {
            return Err("Navigation target identity and membership disagree".into());
        }
        match entry.kind.as_str() {
            "contents"
                if !entry.source_ranges.is_empty()
                    && !entry.source_line_ids.is_empty()
                    && entry.witness_id.is_none() => {}
            "bookmark" if entry.source_ranges.is_empty() && entry.source_line_ids.is_empty() => {
                let witness = entry
                    .witness_id
                    .as_deref()
                    .and_then(|id| native_navigation.get(id))
                    .ok_or("Bookmark has no native navigation witness")?;
                let parent_witness = entry
                    .parent_id
                    .as_deref()
                    .and_then(|id| navigation.get(id))
                    .and_then(|parent| parent.witness_id.as_deref());
                if parent_witness != witness.parent_id.as_deref() {
                    return Err("Bookmark hierarchy differs from native navigation witness".into());
                }
            }
            _ => {
                return Err("Navigation lacks printed contents or native bookmark evidence".into())
            }
        }
        if entry.title_ranges.iter().any(|title| {
            !entry
                .source_ranges
                .iter()
                .any(|source| source.start <= title.start && title.end <= source.end)
        }) {
            return Err("Navigation title outside printed entry".into());
        }
        for (ranges, ids) in [
            (&entry.source_ranges, &entry.source_line_ids),
            (&entry.target_ranges, &entry.target_line_ids),
        ] {
            if !structure.source_fragments.is_empty() {
                if ids.iter().any(|id| !source_ids.contains(id))
                    || ranges.iter().any(|range| {
                        !structure.source_fragments.iter().any(|fragment| {
                            ids.contains(&fragment.id)
                                && fragment.range.start <= range.start
                                && range.end <= fragment.range.end
                        })
                    })
                {
                    return Err("Navigation ranges disagree with source fragment identities".into());
                }
            }
        }
        if !entry.target_ranges.is_empty() {
            let inside = |members: &[ScalarRange]| {
                let mut members = members.to_vec();
                ScalarRange::normalize_union(&mut members);
                entry.target_ranges.iter().all(|target| members.iter()
                    .any(|range| range.start <= target.start && target.end <= range.end))
            };
            let document_scopes: HashSet<_> = structure.scope_annotations.iter()
                .filter(|scope| scope.kind == crate::StructureScopeKind::Document).map(|scope| scope.id.as_str()).collect();
            let body = structure.nodes.iter().any(|node| navigation_body_ranges(node, document_scopes.contains(node.id.as_str())).is_some_and(inside))
                || structure.scope_annotations.iter().any(|scope| scope.kind == crate::StructureScopeKind::Document
                    && inside(&scope.title_ranges));
            let contents = structure.navigation.iter().filter(|source| source.kind == "contents")
                .flat_map(|source| &source.source_ranges).any(|source| entry.target_ranges.iter()
                    .any(|target| source.start < target.end && target.start < source.end));
            let excluded = structure.nodes.iter().filter(|node| node.role.is_some_and(|role| !navigation_body_role(role)))
                .flat_map(|node| if node.ranges.is_empty() { std::slice::from_ref(&node.range) } else { &node.ranges })
                .any(|source| entry.target_ranges.iter().any(|target| source.start < target.end && target.start < source.end));
            if !body || contents || excluded {
                return Err("Confirmed navigation target must name body membership or a document title outside printed contents, furniture and page labels".into());
            }
        }
        let mut seen = HashSet::from([entry.id.as_str()]);
        let mut parent = entry.parent_id.as_deref();
        while let Some(id) = parent {
            if !seen.insert(id) {
                return Err("Navigation hierarchy contains a cycle".into());
            }
            parent = navigation
                .get(id)
                .ok_or("Navigation parent is absent")?
                .parent_id
                .as_deref();
        }
    }
    let mut children = vec![Vec::new(); structure.nodes.len()];
    let mut roots = VecDeque::new();
    for (i, node) in structure.nodes.iter().enumerate() {
        if let Some(parent) = &node.parent_id {
            let parent = *positions.get(parent).ok_or("Structural parent is absent")?;
            if node.ownership.as_ref().zip(structure.nodes[parent].ownership.as_ref())
                .is_some_and(|(owner, parent_owner)| owner != parent_owner)
                && !node.kind.is_grid_child_of(structure.nodes[parent].kind) {
                return Err(
                    "Structural parent crosses document, quotation or reading-stream scope".into(),
                );
            }
            children[parent].push(i);
        } else {
            roots.push_back((i, 1));
        }
    }
    let mut order = Vec::with_capacity(structure.nodes.len());
    let mut levels = BTreeMap::new();
    while let Some((i, level)) = roots.pop_front() {
        levels.insert(structure.nodes[i].id.clone(), level);
        order.push(i);
        roots.extend(children[i].iter().map(|child| (*child, level + 1)));
    }
    if order.len() != structure.nodes.len() {
        return Err("Structural parents contain a cycle".into());
    }
    let mut spans = structure
        .nodes
        .iter()
        .map(|node| {
            if node.ranges.is_empty() {
                vec![node.range]
            } else {
                node.ranges.clone()
            }
        })
        .collect::<Vec<_>>();
    let same_text = structure
        .rendered_text
        .as_deref()
        .is_none_or(|value| value == structure.text);
    let own_rendered = structure
        .nodes
        .iter()
        .enumerate()
        .map(|(i, node)| {
            if same_text {
                return Ok(spans[i].clone());
            }
            if !node.rendered_membership.is_empty() {
                return Ok(node.rendered_membership.clone());
            }
            if node.rendered_range == Some(ScalarRange { start: 0, end: 0 })
                && node.rendered_ranges.is_empty()
            {
                return Ok(Vec::new());
            }
            if matches!(
                node.kind,
                crate::NodeKind::Heading | crate::NodeKind::Section
            ) || node.role == Some(PrimaryRole::Heading)
            {
                return Err("Rendered sections require their own logical membership".to_owned());
            }
            if !node.rendered_ranges.is_empty() {
                return Ok(node.rendered_ranges.clone());
            }
            node.rendered_range
                .map(|range| vec![range])
                .ok_or_else(|| "Missing source-to-manuscript membership".to_owned())
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut rendered_extents = own_rendered.clone();
    for i in order.into_iter().rev() {
        ScalarRange::normalize_union(&mut spans[i]);
        ScalarRange::normalize_union(&mut rendered_extents[i]);
        if let Some(parent) = &structure.nodes[i].parent_id {
            let parts = spans[i].clone();
            spans[positions[parent]].extend(parts);
            let parts = rendered_extents[i].clone();
            rendered_extents[positions[parent]].extend(parts);
        }
    }
    let mut note_ids = HashSet::new();
    for note in &structure.notes {
        if note.id.is_empty() || !note_ids.insert(&note.id) {
            return Err("Empty or duplicate note identity".into());
        }
        if note.node_id.as_ref().is_some_and(|id| !positions.contains_key(id)) {
            return Err("Annotation replacement removed a note's node".into());
        }
        if note.ownership.as_ref().is_some_and(|owner| owner.document_id.is_empty()) {
            return Err("Note document ownership is empty".into());
        }
        for range in [&note.label_range, &note.body_range].into_iter()
            .chain(note.label_ranges.iter()).chain(note.body_ranges.iter())
            .chain(note.references.iter().map(|reference| &reference.range))
            .chain(note.primary_reference.iter()) {
            if range.start > range.end || text.slice_utf16(range.start..range.end).is_none() {
                return Err("Note span outside source text".into());
            }
        }
    }
    for (i, node) in structure.nodes.iter_mut().enumerate() {
        node.level = levels.get(&node.id).copied();
        node.extent = spans[i].clone();
        node.rendered_membership = own_rendered[i].clone();
        node.rendered_ranges = if matches!(
            node.kind,
            crate::NodeKind::Heading | crate::NodeKind::Section
        ) || node.role == Some(PrimaryRole::Heading)
        {
            rendered_extents[i].clone()
        } else {
            own_rendered[i].clone()
        };
        node.rendered_range =
            Some(envelope(&node.rendered_ranges).unwrap_or(ScalarRange { start: 0, end: 0 }));
    }
    let extents = structure
        .nodes
        .iter()
        .enumerate()
        .map(|(i, node)| (node.id.clone(), spans[i].clone()))
        .collect();
    structure
        .refresh_revision()
        .map_err(|error| error.to_string())?;
    Ok(AppliedStructureAnnotations {
        structure,
        levels,
        extents,
    })
}
