# Legal Structure Parser

Fast, deterministic legal-document structure for Rust applications.

The crate produces one provider-neutral `DocumentStructure` from agreements,
provider markup, court decisions, or extracted document text. Format adapters
can preserve authoritative PDF and DOCX signals while
feeding that same structure engine. The result owns its text, nodes, origins,
notes, tables, and cross-references; a per-document `DocumentQuery` lazily
indexes only the query operations a caller uses.

## Use

```toml
[dependencies]
legal-structure = { git = "https://github.com/eliziff/legal-structure-parser.git", features = ["structure-inference"] }
```

```rust
use legal_structure::analyze_instrument;

let document = analyze_instrument("1. Definitions\n...", "agreement-1".into(), &[], true)?;
```

No features are enabled by default. Enable only the public operations needed:

| Feature | Public API |
| --- | --- |
| `structure-inference` | Instrument analysis and generic candidate resolution |
| `document-query` | `DocumentQuery` navigation and text lookup |
| `provider-text` | `provider_text_document_structure` for provider text and ordered section evidence (enables structure inference) |
| `native-markup` | `analyze_native_markup` (enables structure inference) |
| `footnote-pairing` | `pair_numbered_footnotes` pairs numbered bodies with in-text references; opt into proposition sentences and preceding passages with `include_context` |
| `citator` | Citation matching, lookup keys, and excerpt classification |
| `quote-verification` | Grounded quote checks (enables document queries) |

Run the focused checks with:

```sh
cargo test -p legal-grammar-tables --locked
cargo check -p legal-structure --locked
cargo test -p legal-structure --locked --all-features
cargo check -p legal-structure-python --locked
```

## Design

- Rust owns detection, normalization, offsets, structure, and navigation.
- `ScalarText` is the single byte/scalar/UTF-16 coordinate primitive.
- Provider markup is preserved when authoritative and adapted into the common
  document only once.

## Refactor status and remaining assumptions

The standalone-engine refactor has landed. The crate has no dependency on
Beaver, Node, Supabase, or the PDF extraction runtime. PDF extraction, OCR,
geometry, fetching, persistence, and application policy belong to consumers.
The Python binding lives in `python/`; Beaver maintains its Node adapter
outside this repository.

The shared implementation now provides:

- One `DocumentStructure` and internal input/assembly path for native facts
  and inferred structure, with coverage preventing inference from replacing
  authoritative evidence.
- Separate marker-candidate detection and evidence-based role resolution via
  `detect_structure_candidate_runs` and `resolve_structure_graph`.
- Provider-neutral text input through `ProviderTextInput`, including ordered
  section evidence and explicit `require_report_start` policy.
- Standalone numbered footnote pairing, with optional
  proposition/passage context rather than mandatory context derivation.
- Lazy document queries and shared scalar/byte/UTF-16 coordinate conversion.

Primary document profiles remain intentional. A document's own structure is
distinct from numbering inside quotations. A compound record may contain
separately profiled documents within bounded spans; numbering or a style
change alone does not establish such a boundary.

Retained policies and API constraints include:

- Text inference still selects document-wide `DetectionProfile` policies.
  Provider cases select `CaseRootedComplete`; laws select `Legislation`.
  Candidate resolution operates alongside these policies; removing profiles
  is not the objective of the refactor.
- Provider law names containing rules/regulations terminology still enable
  hyphenated-section handling. Report-page inference still recognizes Canadian
  reporter citations. These are retained domain heuristics, not universal
  properties of legal text.
- Candidate evidence and note-pair inputs carry page indexes and line IDs;
  that public boundary still reflects PDF witnesses.
- Each `DocumentQuery` must be used with one unchanged document. Its lazy
  indexes are not keyed by document identity; create a new query when changing
  documents or revisions.

Unit tests are not corpus-fidelity certification. Changes to these assumptions
need complete output comparisons for the affected instruments, providers,
PDFs, and other format contracts. PDF cached-extraction structure replay and
full extraction/OCR lifecycle validation are separate gates. Downstream
consumers must pin and validate the exact engine revision they ship; a local
path override can exercise a newer engine than a consumer's published Git pin.

When working inside Beaver, reuse its warm adapter target for an engine check:

```sh
cargo check --manifest-path native/legal-structure-node/Cargo.toml --offline
```

Run that command from the Beaver root. The standalone checks above are for a
checkout of this repository.

## License

MIT
