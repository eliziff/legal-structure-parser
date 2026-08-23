#[cfg(feature = "structure-inference")]
use super::inference::{formal_heading, statute_spine};
use super::*;

fn evidence(text: &str, profile: DetectionProfile) -> DocumentInput {
    let range = ScalarRange {
        start: 0,
        end: text.chars().count(),
    };
    DocumentInput {
        schema_version: EVIDENCE_SCHEMA.into(),
        document_id: "test".into(),
        provider: "test".into(),
        #[cfg(feature = "document-query")]
        url: None,
        #[cfg(feature = "document-query")]
        doc_type: None,
        provider_revision: "1".into(),
        profile,
        report_start_page: None,
        require_report_start: false,
        allow_hyphenated_sections: false,
        text: text.into(),
        text_sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
        source_sha256: None,
        offset_unit: "unicode-scalar".into(),
        scope: Scope {
            kind: ScopeKind::Complete,
            excerpt_of: None,
        },
        origins: vec![Origin {
            id: "native".into(),
        }],
        native_claims: Vec::new(),
        coverage: [
            EvidenceKind::Paragraph,
            EvidenceKind::Prose,
            EvidenceKind::Page,
            EvidenceKind::Section,
            EvidenceKind::Heading,
            EvidenceKind::Footnote,
            EvidenceKind::Endnote,
        ]
        .into_iter()
        .map(|kind| Coverage {
            kind,
            range,
            state: CoverageState::Absent,
        })
        .collect(),
        exclusions: Vec::new(),
        paragraph_breaks: Vec::new(),
    }
}

#[cfg(feature = "document-query")]
#[test]
fn section_locator_prefixes_are_delimited() {
    for (input, expected) in [
        ("section 34", "sec34"),
        ("s. 34(1)(a)", "sec34(1)(a)"),
        ("sec34", "sec34"),
        ("section sec34", "sec34"),
        ("Savings", "sectitle:savings"),
        ("Schedule 1", "sectitle:schedule 1"),
    ] {
        assert_eq!(
            normalize_document_locator(DocumentKind::Section, input),
            expected
        );
    }
}

#[test]
fn validates_scope_and_ranges() {
    let mut value = evidence("abc", DetectionProfile::CaseRootedComplete);
    value.scope = Scope {
        kind: ScopeKind::Excerpt,
        excerpt_of: Some("whole".into()),
    };
    assert!(value.validate().is_err());
    value.profile = DetectionProfile::CaseLossy;
    assert!(value.validate().is_ok());
    value.native_claims.push(NativeClaim {
        id: "bad".into(),
        kind: EvidenceKind::Page,
        label: Some("page1".into()),
        aliases: Vec::new(),
        range: ScalarRange { start: 0, end: 4 },
        origin_id: "native".into(),
        parent_label: None,
        anchor: None,
    });
    assert!(value.validate().is_err());
}

#[test]
#[cfg(feature = "structure-inference")]
fn weighted_numeric_sequence_policies_preserve_lane_rules() {
    let candidate =
        |index, value, position, page, score, start_supported| NumericSequenceCandidate {
            index,
            value,
            position: (position, 0),
            page,
            score,
            start_supported,
        };
    let rooted = select_numeric_sequence(
        vec![
            candidate(0, 1, 0, 0, 1.0, false),
            candidate(1, 2, 0, 0, 100.0, false),
            candidate(2, 2, 1, 0, 1.0, false),
            candidate(3, 3, 2, 0, 1.0, false),
        ],
        NumericSequencePolicy::RootedConsecutive,
    );
    assert_eq!(rooted.indices, [0, 2, 3]);
    assert!((rooted.score - 3.6).abs() < 1e-9);

    let footnotes = select_numeric_sequence(
        vec![
            candidate(10, 1, 0, 1, 1.0, false),
            candidate(11, 3, 1, 1, 1.0, false),
            candidate(12, 4, 2, 2, 1.0, false),
        ],
        NumericSequencePolicy::FootnoteBackbone,
    );
    assert_eq!(footnotes.indices, [10, 11, 12]);
    assert!((footnotes.score - 2.9).abs() < 1e-9);
    assert_eq!(
        select_numeric_sequence(
            vec![candidate(20, 50, 0, 1, 5.0, true)],
            NumericSequencePolicy::FootnoteBackbone
        ),
        NumericSequenceSelection {
            indices: vec![20],
            score: 5.0
        }
    );
}

#[test]
#[cfg(feature = "structure-inference")]
fn joined_page_tokens_are_not_reporter_pages() {
    let graph = derive_structure_evidence(evidence(
        "Quoted text [page624] continues.\nMore [page625] text.\nLast [page626] text.",
        DetectionProfile::CaseRootedComplete,
    ))
    .unwrap();
    assert!(graph.nodes.iter().all(|node| node.kind != NodeKind::Page));
}

#[test]
fn clips_at_complete_native_coverage() {
    let mut value = evidence("0123456789", DetectionProfile::CaseLossy);
    value
        .coverage
        .retain(|row| row.kind != EvidenceKind::Section);
    value.coverage.extend(
        [
            (0, 3, CoverageState::Absent),
            (3, 7, CoverageState::Complete),
            (7, 10, CoverageState::Augment),
        ]
        .map(|(start, end, state)| Coverage {
            kind: EvidenceKind::Section,
            range: ScalarRange { start, end },
            state,
        }),
    );
    assert_eq!(
        value
            .clip_inference(EvidenceKind::Section, ScalarRange { start: 1, end: 9 })
            .unwrap()
            .end,
        3
    );
    assert!(value
        .clip_inference(EvidenceKind::Section, ScalarRange { start: 4, end: 9 })
        .is_none());
    assert_eq!(
        value
            .clip_inference(EvidenceKind::Section, ScalarRange { start: 7, end: 10 })
            .unwrap()
            .end,
        10
    );
}

#[test]
fn native_projection_preserves_claims_without_recovery() {
    let text = "1 Native provision";
    let mut value = evidence(text, DetectionProfile::Legislation);
    value
        .coverage
        .iter_mut()
        .for_each(|row| row.state = CoverageState::Complete);
    value.native_claims.push(NativeClaim {
        id: "native-section".into(),
        kind: EvidenceKind::Section,
        label: Some("sec1".into()),
        aliases: vec!["s1".into()],
        range: ScalarRange {
            start: 0,
            end: text.chars().count(),
        },
        origin_id: "native".into(),
        parent_label: None,
        anchor: Some("section-1".into()),
    });
    let graph = derive_native_structure_evidence(value).expect("valid native evidence");
    assert_eq!(graph.nodes.len(), 1);
    assert_eq!(graph.nodes[0].label.as_deref(), Some("sec1"));
    assert!(matches!(graph.nodes[0].source, Derivation::Native));
}

#[test]
#[cfg(feature = "structure-inference")]
fn native_parent_wins_and_children_survive() {
    let derive = |value| derive_structure_evidence(value).expect("valid fixture evidence");
    let flat = derive(evidence(
        "1 First provision\n2 Second provision",
        DetectionProfile::Legislation,
    ));
    assert!(flat
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Section)
        .all(|node| node
            .content_start
            .is_some_and(|at| node.range.start <= at && at <= node.range.end)));
    let text = "1 (1) Parent words\n(2) Child words";
    let mut value = evidence(text, DetectionProfile::Legislation);
    value
        .coverage
        .iter_mut()
        .find(|row| row.kind == EvidenceKind::Section)
        .unwrap()
        .state = CoverageState::Augment;
    value.native_claims.push(NativeClaim {
        id: "native-section".into(),
        kind: EvidenceKind::Section,
        label: Some("sec1".into()),
        aliases: Vec::new(),
        range: ScalarRange {
            start: 0,
            end: text.chars().count(),
        },
        origin_id: "native".into(),
        parent_label: None,
        anchor: None,
    });
    let graph = derive(value);
    assert_eq!(
        graph
            .nodes
            .iter()
            .filter(|node| node.label.as_deref() == Some("sec1"))
            .count(),
        1
    );
    assert!(["sec1(1)", "sec1(2)"].into_iter().all(|label| graph
        .nodes
        .iter()
        .any(|node| node.label.as_deref() == Some(label))));
    let text = "1 (1) Parent provision.\n(a) First paragraph.\n(i) First subparagraph.\n(ii) Second subparagraph.\n(a) Duplicate paragraph marker.\n(b) Second paragraph.\n(2) Sibling subsection.\n2 Next provision.";
    let law = derive(evidence(text, DetectionProfile::Legislation));
    let node = |label| {
        law.nodes
            .iter()
            .find(|node| node.label.as_deref() == Some(label))
            .unwrap()
    };
    assert_eq!(node("sec1(1)").range.start, 0);
    assert_eq!(
        node("sec1(1)").range.end,
        text[..text.find("(a) First").unwrap()].chars().count()
    );
    assert_eq!(
        node("sec1(1)(a)").parent_id.as_deref(),
        Some(node("sec1").id.as_str())
    );
    assert_eq!(
        law.nodes
            .iter()
            .filter(|node| node.label.as_deref() == Some("sec1(1)(a)"))
            .count(),
        1
    );

    let criminal = "**231** (4) Parent subsection.\n(a) First paragraph.\n(b) Second paragraph.\n(c) Third paragraph.\n(5) Sibling subsection.";
    let criminal = derive(evidence(criminal, DetectionProfile::Legislation));
    let section = criminal
        .nodes
        .iter()
        .find(|node| node.label.as_deref() == Some("sec231"))
        .unwrap();
    let subsection = criminal
        .nodes
        .iter()
        .find(|node| node.label.as_deref() == Some("sec231(4)"))
        .unwrap();
    assert_eq!(
        subsection.range.end,
        "**231** (4) Parent subsection.\n".chars().count()
    );
    assert!(["a", "b", "c"].into_iter().all(|label| {
        let label = format!("sec231(4)({label})");
        criminal.nodes.iter().any(|node| {
            node.label.as_deref() == Some(label.as_str())
                && node.parent_id.as_deref() == Some(section.id.as_str())
        })
    }));

    let section_map = "**22.1** (a) Parent paragraph.\n(i) First subparagraph.\n(ii) Second subparagraph.\n(b) Sibling paragraph.";
    let section_map = derive(evidence(section_map, DetectionProfile::Legislation));
    let section = section_map
        .nodes
        .iter()
        .find(|node| node.label.as_deref() == Some("sec22.1"))
        .unwrap();
    let paragraph = section_map
        .nodes
        .iter()
        .find(|node| node.label.as_deref() == Some("sec22.1(a)"))
        .unwrap();
    assert_eq!(
        paragraph.range.end,
        "**22.1** (a) Parent paragraph.\n".chars().count()
    );
    assert!(["i", "ii"].into_iter().all(|label| {
        let label = format!("sec22.1(a)({label})");
        section_map.nodes.iter().any(|node| {
            node.label.as_deref() == Some(label.as_str())
                && node.parent_id.as_deref() == Some(section.id.as_str())
        })
    }));
    let instrument = derive(evidence(
            "Section 1.01 Nested.\n(a) alpha\n(i) roman\n(A) upper\n(I) roman\n(1) digit\nSection 1.02 Doubled.\n(a) alpha\n(z) jump\n(aa) double\n(bb) double",
            DetectionProfile::Instrument,
        ));
    assert!(instrument.nodes.iter().any(|node| {
        node.label.as_deref() == Some("sec1.01(a)(i)(A)(I)(1)")
            && node.content_start.is_some()
            && node.parent_id.is_some()
    }));
    assert!(instrument
        .nodes
        .iter()
        .any(|node| node.label.as_deref() == Some("sec1.02(bb)")));
}

#[test]
#[cfg(feature = "structure-inference")]
fn instrument_heads_keep_token_and_heading_boundaries() {
    let text = concat!(
        "PART 4.20(a) of the Disclosure Schedule\n",
        "EXHIBIT IV   45\n",
        "SCHEDULE 14D-9 37\n",
        "EXHIBIT III Valid Heading\n",
    );
    let graph = derive_structure_evidence(evidence(text, DetectionProfile::Instrument))
        .expect("valid instrument evidence");
    let labels = graph
        .nodes
        .iter()
        .filter_map(|node| node.label.as_deref())
        .collect::<Vec<_>>();
    assert!(labels.contains(&"part4"));
    assert!(labels.contains(&"exhiii"));
    assert!(!labels
        .iter()
        .any(|label| matches!(*label, "exhi" | "sched14")));
}

#[test]
#[cfg(feature = "structure-inference")]
fn typed_hierarchy_candidates_use_the_production_detector() {
    let runs = detect_structure_candidate_runs(
        "Section 1.01 Opening.\n(a) First clause.\n(b) Second clause.\nSection 1.02 Closing.",
    );
    let sections = runs
        .iter()
        .filter(|run| run.grammar == CandidateGrammar::Hierarchy)
        .flat_map(|run| &run.markers)
        .collect::<Vec<_>>();
    assert_eq!(
        sections
            .iter()
            .map(|section| section.grammar_value.as_str())
            .collect::<Vec<_>>(),
        ["sec1.01", "sec1.01(a)", "sec1.01(b)", "sec1.02"]
    );
    assert_eq!(
        sections[1].parent_candidate_id.as_deref(),
        Some(sections[0].id.as_str())
    );
}

#[test]
#[cfg(feature = "structure-inference")]
fn bare_section_label_without_inline_content_is_safe() {
    let _ = detect_structure_candidate_runs("1\nProvision text.\n2\nMore text.\n3\nFinal text.");
}

#[test]
#[cfg(feature = "structure-inference")]
fn raw_numeric_candidates_keep_late_and_gapped_runs_for_resolution() {
    let runs = detect_structure_candidate_runs(
        "7. First excerpt paragraph.\n9. A gap remains visible.\n12. Another item.",
    );
    let run = runs
        .iter()
        .find(|run| run.grammar == CandidateGrammar::Numeric)
        .expect("numeric candidate run");
    assert!(!run.rooted);
    assert!(!run.consecutive);
    assert_eq!(
        run.markers
            .iter()
            .map(|marker| marker.grammar_value.as_str())
            .collect::<Vec<_>>(),
        ["7", "9", "12"]
    );
}

#[test]
#[cfg(feature = "structure-inference")]
fn typed_evidence_resolves_numeric_prose_and_reports_each_run() {
    let text = "1. First paragraph.\n2. Second paragraph.";
    let run = detect_structure_candidate_runs(text)
        .into_iter()
        .find(|run| run.grammar == CandidateGrammar::Numeric && run.rooted && run.consecutive)
        .expect("rooted numeric candidate run");
    let evidence = run
        .markers
        .iter()
        .enumerate()
        .map(|(index, candidate)| CandidateEvidenceV2 {
            candidate_id: candidate.id.clone(),
            page_indexes: vec![0],
            line_ids: vec![format!("line-{index}")],
            observations: vec![CandidateObservationV2::BodyProseFlow],
        })
        .collect::<Vec<_>>();
    let graph = resolve_structure_graph(
        "numeric".to_owned(),
        text,
        None,
        Vec::new(),
        &[run],
        &evidence,
        &[],
        Vec::new(),
    )
    .expect("valid typed evidence");
    assert_eq!(
        graph
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Paragraph)
            .count(),
        2
    );
    assert_eq!(graph.diagnostics.len(), 1);
    assert_eq!(graph.diagnostics[0].code, "structure_run_resolved");
}

#[test]
#[cfg(feature = "structure-inference")]
fn contents_and_transcript_number_evidence_force_abstention() {
    let text = "1. First paragraph.\n2. Second paragraph.";
    let run = detect_structure_candidate_runs(text)
        .into_iter()
        .find(|run| run.grammar == CandidateGrammar::Numeric && run.rooted && run.consecutive)
        .expect("rooted numeric candidate run");
    for exclusion in [
        CandidateObservationV2::ContentsRow,
        CandidateObservationV2::IndexRow,
        CandidateObservationV2::TranscriptLineNumber,
    ] {
        let evidence = run
            .markers
            .iter()
            .enumerate()
            .map(|(index, candidate)| CandidateEvidenceV2 {
                candidate_id: candidate.id.clone(),
                page_indexes: vec![0],
                line_ids: vec![format!("line-{index}")],
                observations: vec![CandidateObservationV2::BodyProseFlow, exclusion],
            })
            .collect::<Vec<_>>();
        let resolved = resolve_structure_candidates(std::slice::from_ref(&run), &evidence)
            .expect("valid exclusion evidence");
        assert!(resolved.iter().all(|candidate| candidate.role.is_none()));
        assert!(resolved
            .iter()
            .all(|candidate| candidate.proof.rule == ResolutionRuleV2::DirectExclusion));
    }
}

#[test]
#[cfg(feature = "structure-inference")]
fn local_candidate_parent_ids_create_honest_list_items() {
    let text = "Section 1\n(a) item";
    let length = text.chars().count();
    let run = StructureCandidateRun {
        id: "hierarchy-1".to_owned(),
        grammar: CandidateGrammar::Hierarchy,
        range: ScalarRange {
            start: 0,
            end: length,
        },
        rooted: true,
        consecutive: true,
        markers: vec![
            StructureMarkerCandidate {
                id: "section".to_owned(),
                range: ScalarRange {
                    start: 0,
                    end: length,
                },
                marker_range: ScalarRange { start: 0, end: 9 },
                label: "Section 1".to_owned(),
                grammar_value: "sec1".to_owned(),
                parent_candidate_id: None,
                level: 0,
                content_start: 9,
            },
            StructureMarkerCandidate {
                id: "item".to_owned(),
                range: ScalarRange {
                    start: 10,
                    end: length,
                },
                marker_range: ScalarRange { start: 10, end: 14 },
                label: "(a)".to_owned(),
                grammar_value: "1:1".to_owned(),
                parent_candidate_id: Some("section".to_owned()),
                level: 1,
                content_start: 14,
            },
        ],
    };
    let evidence = vec![
        CandidateEvidenceV2 {
            candidate_id: "section".to_owned(),
            page_indexes: vec![0],
            line_ids: vec!["heading".to_owned()],
            observations: vec![CandidateObservationV2::SectionHeading],
        },
        CandidateEvidenceV2 {
            candidate_id: "item".to_owned(),
            page_indexes: vec![0],
            line_ids: vec!["item".to_owned()],
            observations: vec![CandidateObservationV2::ListItemLayout],
        },
    ];
    let graph = resolve_structure_graph(
        "hierarchy".to_owned(),
        text,
        None,
        Vec::new(),
        &[run],
        &evidence,
        &[],
        Vec::new(),
    )
    .expect("valid hierarchy evidence");
    let section = graph
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Section)
        .unwrap();
    let item = graph
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::ListItem)
        .unwrap();
    assert_eq!(
        graph
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Section)
            .count(),
        1
    );
    assert_eq!(section.locator_kind.as_deref(), Some("section"));
    assert_eq!(item.parent_id.as_deref(), Some(section.id.as_str()));
    assert!(!graph.nodes.iter().any(|node| node.kind == NodeKind::List));
}

#[test]
#[cfg(feature = "structure-inference")]
fn paired_note_claim_keeps_note_source_identity() {
    let text = "Body ref 1.\n1 Note body.";
    let reference_start = text.find('1').unwrap();
    let label_start = text.rfind('1').unwrap();
    let pair = NotePairClaimV2 {
        pair_id: "pair-1".to_owned(),
        kind: NoteKindV2::Footnote,
        label: TextAnchorV2 {
            range: ScalarRange {
                start: label_start,
                end: label_start + 1,
            },
            page_index: 1,
            line_id: "note-line".to_owned(),
        },
        body: NoteBodyV2 {
            range: ScalarRange {
                start: label_start,
                end: text.chars().count(),
            },
            page_indexes: vec![1],
            line_ids: vec!["note-line".to_owned()],
        },
        references: vec![
            TextAnchorV2 {
                range: ScalarRange {
                    start: reference_start,
                    end: reference_start + 1,
                },
                page_index: 0,
                line_id: "body-line".to_owned(),
            },
            TextAnchorV2 {
                range: ScalarRange {
                    start: reference_start,
                    end: reference_start + 1,
                },
                page_index: 0,
                line_id: "body-line".to_owned(),
            },
        ],
    };
    let graph = resolve_structure_graph(
        "notes".to_owned(),
        text,
        None,
        Vec::new(),
        &[],
        &[],
        &[pair],
        Vec::new(),
    )
    .expect("valid note claim");
    let note = graph
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Footnote)
        .unwrap();
    assert_eq!(note.anchor.as_deref(), Some("pair-1"));
    assert_eq!(note.page_indexes, [1]);
    assert_eq!(note.line_ids, ["note-line"]);
}

#[test]
#[cfg(feature = "structure-inference")]
fn short_root_preserves_crlf_offset_semantics() {
    let labels = statute_spine(
        &ScalarText::new("1\r\n\r\n________________\r\n2\r\n\r\n________________\r\n"),
        false,
    )
    .into_iter()
    .map(|mark| mark.label)
    .collect::<Vec<_>>();
    assert_eq!(labels, ["1", "2"]);
}

#[test]
#[cfg(feature = "structure-inference")]
fn heading_and_utf16_edges_match_javascript() {
    assert!(formal_heading("Qualified Privilege"));
    assert!(!formal_heading("*429"));
    assert!(!formal_heading("() Heading"));
    let case = "[1] This opening paragraph contains enough ordinary words to establish substantive reasons for decision.\nAll Canadian people affected by the breach [1]\nClass Period\n[2] This second paragraph contains enough ordinary words to establish substantive reasons for decision.\n[3] This third paragraph contains enough ordinary words to establish substantive reasons for decision.\n[4] This fourth paragraph contains enough ordinary words to establish substantive reasons for decision.\n[5] This fifth paragraph contains enough ordinary words to establish substantive reasons for decision.";
    let case_graph =
        derive_structure_evidence(evidence(case, DetectionProfile::CaseRootedComplete))
            .expect("valid case evidence");
    assert_eq!(
        case_graph
            .nodes
            .iter()
            .find(|node| node.label.as_deref() == Some("par1"))
            .unwrap()
            .range
            .end,
        case[..case.find("[1]\nClass Period").unwrap()]
            .chars()
            .count()
    );
    let journal = derive_structure_evidence(evidence(
        "\u{85}alpha\n\n\u{feff}beta",
        DetectionProfile::Journal,
    ))
    .expect("valid journal evidence");
    assert_eq!(
        journal
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Prose)
            .map(|node| node.range.start)
            .collect::<Vec<_>>(),
        [0, 9]
    );
}

#[test]
#[cfg(feature = "structure-inference")]
fn bare_label_alone_extends_substantive_statute_spines() {
    let labels = |text: &str| {
        statute_spine(&ScalarText::new(text), false)
            .into_iter()
            .map(|mark| mark.label)
            .collect::<Vec<_>>()
    };
    assert_eq!(labels("### Interpretation\n85F\nProvision text continues on the next line.\n85G Next provision.\n86 Final provision."), ["85F", "85G", "86"]);
    assert_eq!(labels("1 This Act may be cited as the Example Act.\n2 The following definitions apply in this Act.\n3\nApplication\nThis Act applies to every person in the territory.\n4 The Minister may make regulations."), ["1", "2", "3", "4"]);
}

#[test]
#[cfg(feature = "structure-inference")]
fn dotterm_inline_child_preserves_bare_spine_precedence() {
    let body = "This section provides for the administration of the enactment in force across the territory.";
    let text = format!("1. There is established a board. {body}\n2.(1) A term has the prescribed meaning. {body}\n2.1. This inserted provision governs administration. {body}\n3. The Minister may make regulations. {body}\n4. This Act comes into force. {body}");
    let graph = derive_structure_evidence(evidence(&text, DetectionProfile::Legislation))
        .expect("valid fixture evidence");
    let top = graph
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Section && node.parent_id.is_none())
        .filter_map(|node| node.label.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(top, ["sec1", "sec2", "sec2.1", "sec3", "sec4"]);
    assert!(graph
        .nodes
        .iter()
        .any(|node| node.label.as_deref() == Some("sec2(1)") && node.parent_id.is_some()));

    let ontario = "1 A person is exempted if the person satisfies the following:\n1. The person is registered with a regulatory authority.\n2. A regulatory authority has not refused the person.\n3. A finding of misconduct has not been made.\n4. The person is not the subject of any proceeding.\n5. The person has submitted an application.\n2 A person who is exempted must notify the College.\n3 Omitted (provides for coming into force).";
    assert_eq!(
        statute_spine(&ScalarText::new(ontario), false)
            .into_iter()
            .map(|mark| mark.label)
            .collect::<Vec<_>>(),
        ["1", "2", "3"]
    );
}
