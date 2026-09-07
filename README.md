# Legal Structure Parser

Deterministic legal-document structure and exact queries for Rust applications.
The `legal-structure` crate produces a provider-neutral `DocumentStructure` from
extracted text or authoritative format evidence. The document owns its text,
nodes, origins, notes, tables and cross-references; `DocumentQuery` builds indexes
lazily for the operations a caller uses.

## Use

```toml
[dependencies]
legal-structure = { git = "https://github.com/eliziff/legal-structure-parser.git", features = ["structure-inference"] }
```

No features are enabled by default. Select capabilities in the consumer's
manifest and pin the exact revision validated for a release.

| Feature | Capability |
| --- | --- |
| `structure-inference` | Instrument analysis, marker candidates and evidence-based role resolution |
| `document-query` | Navigation and exact text lookup |
| `provider-text` | Provider text and ordered section evidence; enables inference |
| `native-markup` | Native markup analysis; enables inference |
| `footnote-pairing` | Numbered bodies and in-text references; optional proposition/passage context |
| `citator` | Citation matching, lookup keys and excerpt classification |
| `quote-verification` | Grounded quotation checks; enables document queries |
| `journal` | Journal support |

[Cargo.toml](Cargo.toml) defines feature dependencies; [src/](src/) owns the public
API. Shared grammar tables live in [grammar/](grammar/), and the Python binding
lives in [python/](python/).

## Ownership and limits

This repository owns legal detection, normalization, offsets, structure and
navigation. `ScalarText` is the shared byte/scalar/UTF-16 coordinate primitive.
Authoritative provider or format facts are preserved rather than overwritten by
inference. `ProviderTextInput` carries ordered section evidence and the explicit
`require_report_start` policy.

PDF extraction, geometry and OCR belong to
[Legal PDF Parser](https://github.com/eliziff/legal-pdf-parser). Fetching,
persistence, permissions and application policy belong to consumers such as
[Beaver](https://github.com/eliziff/Beaver) and
[Legal Pinpointer](https://github.com/eliziff/legal-pinpointer). Beaver's Node
adapter is maintained in Beaver, not here.

Primary document profiles remain intentional: a document's own paragraphs and
sections are distinct from numbering inside quoted legislation, agreements or
decisions. Candidate detection and role resolution do not replace that ownership.
Compound records need reviewed constituent boundaries; a numbering restart, tab,
page break or style change alone does not establish another document. Ambiguity
must preserve exact page/text access, not manufacture a constituent identity.

Retained implementation constraints:

- Text inference uses document-wide `DetectionProfile` policies, including
  `CaseRootedComplete` for provider cases and `Legislation` for laws. Rules and
  regulations can enable hyphenated sections; report-page inference recognizes
  Canadian reporter citations. These are domain heuristics, not universal rules.
- Candidate and note-pairing inputs still carry page indexes and line IDs.
- A `DocumentQuery` belongs to one unchanged document. Create a new query for a
  different document or revision; its lazy indexes are not identity-keyed.

## Validation and planning

From this repository's root:

```sh
cargo check -p legal-structure --locked
cargo test -p legal-grammar-tables --locked
cargo test -p legal-structure --locked --all-features
cargo check -p legal-structure-python --locked
```

Use focused checks during edits. Unit tests are not corpus-fidelity certification:
changes to profiles, quotation ownership or compound boundaries require complete
output comparisons for affected providers and formats. PDF cached-extraction
structure replay and the full extraction/OCR lifecycle are separate gates.
A consumer's local path override does not validate its published Git pin.

Cross-project priorities and remaining integration gates belong to
[Beaver's master plan](https://github.com/eliziff/Beaver/blob/main/docs/roadmap/master-plan.md),
not a second implementation backlog here. Experiments are not shipped APIs.

## License

[MIT](LICENSE). Consumer applications and third-party assets retain their own
licenses.
