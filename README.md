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

```rust
use legal_structure::analyze_instrument;

let document = analyze_instrument("1. Definitions\n...", "agreement-1".into(), &[], true)?;
```

No features are enabled by default. Enable the operations your application needs:

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

## Working with documents

`ScalarText` converts between byte, Unicode scalar and UTF-16 offsets. Structure
inferred from text preserves authoritative facts supplied by a format adapter.
Create a new `DocumentQuery` when the document or its revision changes.

Text inference uses legal-document profiles and domain heuristics. Ambiguous
numbering and boundaries in combined records may require format evidence;
exact text lookup remains available independently of inferred paragraph labels.

For PDF extraction and OCR, see
[Legal PDF Parser](https://github.com/eliziff/legal-pdf-parser).

## Development

From this repository's root:

```sh
cargo check -p legal-structure --locked
cargo test -p legal-grammar-tables --locked
cargo test -p legal-structure --locked --all-features
cargo check -p legal-structure-python --locked
```

## License

[MIT](LICENSE). Consumer applications and third-party assets retain their own
licenses.
