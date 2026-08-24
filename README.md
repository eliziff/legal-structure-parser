# Legal Structure

Fast, deterministic legal-document structure for Rust applications.

The crate produces one provider-neutral `DocumentStructure` from agreements,
provider markup, court decisions, journal articles, or extracted document
text. Format adapters can preserve authoritative PDF and DOCX signals while
feeding that same structure engine. The result owns its text, nodes, origins,
notes, tables, and cross-references; a per-document `DocumentQuery` lazily
indexes only the query operations a caller uses.

## Use

```toml
[dependencies]
legal-structure = { git = "https://github.com/eliziff/legal-structure.git", features = ["structure-inference"] }
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
| `a2aj` | `a2aj_document_structure` (enables structure inference) |
| `native-markup` | `analyze_native_markup` (enables structure inference) |
| `journal` | Journal JSONL/text structure and footnote pairing |
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

## License

MIT
