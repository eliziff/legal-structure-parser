# CourtListener corpus iteration

## Current deliverable and closure plan

The deliverable is the existing Rust legal structure parser recovering and
querying CourtListener's available legal structure. Supplied boundaries,
hierarchy, numbering, note relationships, pagination and source identities are
authoritative. Supplementation fills missing structure, retains its provenance
and yields to supplied evidence. Complete compressed originals and metadata
remain the preservation baseline; the measured reconstruction recipes did not
improve compressed size. Storage experimentation is closed for this iteration.

Bulk iteration is the primary workflow. Compare each combined engine candidate
across the retained diverse originals, then across the pinned full snapshot
when the remaining coverage question requires it. Report supplied roles,
recovered roles, unexplained differences and actionable source forms by
rendition/family; processing counts alone are not acceptance. Small cached
address cohorts provide supporting navigation evidence. They do not define
the breadth target or establish corpus-wide link yield.

Preserving explicit source declarations is basic parser work. Recovery priorities
are missing passage partitions, wrongly merged ranges, lost writing ownership
and unresolved note/reference relationships. The existing corpus runner now
observes actual Paragraph/Prose query coverage, uncovered UTF-16 ranges, large
passage counts and writing-boundary conflicts. Sections and viewer fallback
text do not count as paragraph coverage. These observations guide source review;
they do not grade legal correctness. Anonymous Prose already has paragraph-query
locators, so absence of a numbered Paragraph node alone is not a coverage defect.

Keep one current issue table here. Historical measurements below and in
[FINDINGS.md](FINDINGS.md) provide evidence, rather than an accumulating backlog.
An unmeasured future format is a coverage limit; it does not reopen a resolved
defect. A source-declared opinion boundary can bound an inferred paragraph
without asserting that each physical P is a canonical legal paragraph.

| Order | Concrete work | Acceptance and disposition |
| --- | --- | --- |
| 1 | Repair inferred paragraphs extending beyond explicit matched opinion boundaries. | **Passed on retained originals:** 25 focused end-only corrections and 19 additional source-supported writing upper bounds. The broader comparison covers 40,211 complete originals / 89,662 nonempty renditions with no source, text or literal-graph loss. These bounds do not establish canonical paragraph ends or resolve overlapping nested writings; that next repair is listed below. |
| 2 | Recover 24 Columbia declared opinion regions whose actual matching close was lost when a physical P closed; the original six examples belong to this family. | **Focused source comparison passed:** all 24 declared body ranges and opinion views match independently frozen token pairs and intervals; 17 normally matched body declarations remain controls. All 71 combined complete records match, including 34 unchanged records. The physical graph is preserved; no byline-based ownership or publisher anchor is invented. |
| 3 | Recover and recheck the remaining identifiable processing failures. | **All 18 identifiable rows / 32 historically failed fields now pass.** The final two complete original rows process all four nonempty fields with the compiled `b2b6ceaebe24` candidate, including all three historically failed fields. Exact raw CSV records and all 22 source values match; empty failure-ledger accounting is complete. The 181 failure identities omitted by the historical stream remain unknown. |
| 4 | Retain actionable evidence for every future processing failure. | **Implemented in the Rust aggregate and canonical caller:** every failed field retains its identity/error/hash and complete original row, deduplicated by row. The shared byte cap includes the failure pack, index and receipts; inability to retain evidence ends the run explicitly. Empty-ledger accounting passed on the six recovered rows. An authentic future failure is still needed to exercise the nonempty capture branch. |
| 5 | Close the 350 additional structural changes found in the retained diverse corpus. | **Exact source comparison passed:** 663 native Sections, 350 source-opinion views, 375 source-ID Section queries and all 350 outlines. Three source-backed outline entries in two fields are additions; 348 outlines remain exact. Source, text and the complete generic graph remain unchanged. This does not claim unavailable historical whole-node-vector parity. |
| 6 | Check the combined parser and usable source addresses across existing diverse originals and cached pages. | **Combined retained-corpus run completed:** all 56 changed fields are source-accounted, including the 19 additional upper bounds above. Existing cached source-address navigation also passes with the original waits and canonical metadata. This is supporting navigation coverage, not corpus-wide link yield. Anchorless body regions do not acquire URL fragments from Native node IDs. |
| 7 | Stop inferred paragraphs from consuming separately supplied nested writings. | **Rust repair in progress.** A source pass over all retained originals found 18 matched nested-writing pairs in 15 fields. Their separate judicial authorship is supplied, and every child begins outside the starting physical P. The repair bounds inferred ranges at a later writing's supplied start without equating physical P ends with canonical paragraph ends. Outer votes, notes and separately numbered paragraphs remain in the representation. Compilation and the combined corpus run remain required. |
| 8 | Preserve explicit SYLLABUS regions as native sections. | **Rust repair in progress.** The whole-source inventory contains this recurring legal preface form. Use actual matching source tokens, including the existing custom-section recovery when a physical P close drained the generic stack. Preserve original text, graph, hierarchy and source identities; an EOF or unrelated enclosing close does not establish a complete section. |
| 9 | Recover Lawbox note bodies and references where ordinary paragraphs under NOTES contain printed bracketed labels. | **Recovery implementation in progress.** Authentic sources show inline SUP markers and multi-paragraph note bodies without note IDs/classes/hrefs. Retain supplied P/heading structure, add inferred Note roles and scoped associations, and keep repeated or ambiguous labels unresolved. Do not use a physical P end to truncate a continued note. |
| 10 | Recover anonymous passage ranges in CourtListener plain/PRE cases with no supplied or recovered Paragraph/Prose positions. | **Recovery implementation in progress.** The retained observations contain 1,283 such fields, including 941 longer than 10,000 UTF-16 units; 904 of those are plain text. Use actual paragraph layout while retaining wrapped lines and yielding to existing headings, notes and ranges. Double-spaced opinions are controls against naive blank-line splitting. No printed judicial numbering is invented. |

The next full stream is conditional: it must answer a named coverage question
that the existing source inventory and retained corpus cannot answer, after
known recoverable defects are closed. Its outputs must retain every failure
and actionable source witness. It must report supplied-role recovery and
remaining discrepancies, not only attempted-row or successful-execution counts.
The prior stream's 181 omitted failure identities are unrecoverable from its
receipts and are not silently assigned to a known cause.

Finish this candidate when the table's recoverable defects are closed, the
combined real-corpus comparison has no unexplained losses, and source-address
outcomes are measured. Report genuinely ambiguous source boundaries explicitly
without treating them as complete native scopes. This establishes the declared
snapshot/form scope; future unseen documents do not imply universal certainty.
Use only real corpus runs and necessary compilation. Keep existing UCD delays,
retry, navigation and shutdown behavior, the 45 GB cap and retired-pack policy.

## Qualified work and historical evidence

This executable runs the independent legal structure engine against retained
CourtListener renditions. Beaver is one consumer. Inputs and receipts stay in
the owning private benchmark directories, not this repository or scratch.

The qualified executable `8614a307` recovers **33 literal writing
scopes, six nested writing relationships and 30 source bylines** across 17
complete original CourtListener fields. Exact `div.de-writing` declarations
become native sections; `div.de-author` and `span.de-author` remain borrowed
source declarations owned by their nearest writing. Text, ranges, notes and
the full source graph remain exact. These fields supply no new source anchors.

The subsequent qualified executable `29d47d08` also recovers **682 matched
SUMMARY sections** across **86 complete original rendition fields**, with
**682 successful source-anchor / Section range query pairs**. The complete
comparison preserves every old text, outline, opinion component and unrelated
source value. This is source-query qualification; served navigation remains
separate.

The combined candidate `41936a17` preserves that complete SUMMARY comparison
and recovers **34 inferred LDML printed-marker / note-body associations**.
All **71 complete fields** match the independent source expectations, including
the unchanged seven existing references and 63 equality controls. Its outline
correction resolves four independently reproduced failures from two exact
original records without changing their initial text hashes or node counts.
These qualifications establish specific recovery behavior; they do not close
all known structure gaps.

The current producer passes the independent **269 formatted-heading / 728-input Heading contract**. All269 required additions,42 existing heading controls and182 source-negative controls pass; all five historical diagnostics in four fields are closed. Source and text remain exact in all728 records, with no unexpected inferred headings. This is component qualification: the historical whole-model representation differs after later API and structure changes. Private proof: `native-formatted-heading-closure-review/comparison-all728.json` (SHA256 `fdfb1a5d28cfabddd2e43bbe9393dc3bae9bc39b53bd7425bc4f96f95d012a55`).

Earlier qualified producers recover printed pages, correction sections,
headers, note relationships, heading hierarchy and appendix boundaries. Their
actual source comparisons, broader preservation results and measured CLI speed
change are recorded in [FINDINGS.md](FINDINGS.md). The full opinions parser
stream has completed with its frozen `8614a307` producer: **10,903,928 records /
24,163,852 nonempty rendition attempts**, with matching pinned compressed and
decoded source hashes through EOF. **213 attempts panic in the processing
pipeline**; **24,163,639 succeed**, with zero ordinary parser errors. Complete
processing coverage does not establish independent legal correctness or that
every attempted field parsed successfully. Exact recovered failure sources and
later candidate qualification remain separately attributed.

## Expanded public-collection scope

Work now includes every public CourtListener collection. The frozen
**2026-09-30 catalog identifies 33 quarterly CSV tables**, including **32 beyond
the completed opinions census**, with **55 declared SQL joins**. Four retained
metadata originals are reused by hash reference; the **28 uncached CSVs total
2,095,042,446 compressed bytes**. Exact filenames, columns, ETags, hashes and
access boundaries live in the private `all-collections/catalog/` owner. The
[official bulk documentation](https://wiki.free.law/c/courtlistener/help/api/bulk-data/bulk-legal-data)
describes the snapshot and schema formats. Catalog capture establishes source
identities and declared relationships; record and document-body coverage require
separate actual reads.

The historical generic CSV/native witness-run build completed with **stable sources**, producing
`ec540a51`. It includes the generic CSV runner and native runner for all
**40,211 retained opinion witnesses**. The courts record census reached decoded
EOF with **3,361 rows** and complete column, feature and witness coverage; its
independent comparison passed for **all 67,220 cells** and **573 complete stored
witnesses**, including literal CSV records and field hashes. The actual native
inventory of **all 40,211 opinion witnesses / 89,662 nonempty rendition inputs**
completed in **177.051 seconds**, with zero parser errors and no output-cap
omissions. Its observations remain separate from independent semantic gold.
All **33 published quarterly tables** have now reached verified EOF, accounting
for **229,511,178 source rows** including citation and other relation tables.
This includes **10,903,928 opinions**, **10,174,551 opinion clusters** and
**72,998,758 dockets**. Every table's original ordered columns are accounted for;
**15 of the 32 additional tables** retain explicit limits on form/example
coverage. Public mirror bodies and independent legal-semantic qualification
remain separate. The Columbia candidate and the ten-field LDML source-role
comparison are independently qualified.

The qualified LDML change recovers **six opinion bodies, two appendices, six
author declarations and four literal type declarations** across retained HTML
and annotated HTML. All **9,986 source elements** and every other complete record
value stay exact in the ten-field comparison. `source_opinion_components`
now returns borrowed, literal `type_declarations` and
`unowned_type_declarations` when nonempty. New roles retain source ranges and
explicit ancestry; they produce no invented author identity, opinion type,
appendix ownership or browser anchor. See private
`ldml-opinion-components/candidate-comparison.json` and the
[recovery findings](FINDINGS.md).

The qualified parser candidate, executable SHA-256 beginning `d7c21ebf`, also
recovers **50 declared LDML headings and 50 nested sections**, preserves their
**50 source IDs**, and removes **12 exact duplicate inferred headings** while
keeping every other node identity. Heading levels follow actual section
ancestry. All ten complete records, source graphs, outlines and opinion queries
match the independently frozen expectations. A separate eight-field Columbia
comparison qualifies **two properly closed opinion bodies** and leaves six
crossing bodies unresolved. Both comparisons use the same actual producer;
source-ID query correctness does not establish publisher landing.
The broader source comparison covers **all 71 affected fields** found in the
40,211 retained complete opinion witnesses. It preserves **97,459 source
elements**, exact text and source graphs, and qualifies **390 headings, 474
sections, 17 properly closed opinion bodies and 426 source-ID lookups**. The
**24 malformed body scopes** remain generic and unresolved. Its actual Rust
commands take **2.83 seconds** on reusable inputs. This includes 20 source forms
outside the first two comparison cohorts; it is not semantic qualification of
every opinion in the full corpus.
Separately, **all 21 emitted heading links** on two original cached CourtListener
pages pass the existing browser runner's exact-target, opinion-scope, visibility
and natural-landing checks. That is cached-page evidence; current publisher
navigation remains unmeasured for this heading cohort.

The subsequent Rust candidate `16ad273f` recovers **381 declared LDML note
bodies** across **41 original renditions**, including separate literal label
and body ranges for `[N]`, `N` and `N.` forms. Source note markers replace
**18 false inferred paragraph claims** without changing unrelated IDs or ranges.
All 41 complete records match the frozen independent expectations. The existing
query path observes **369 successful note lookups and 12 correctly ambiguous
calls** in **1.24 seconds** of actual Rust execution. These idless notes acquire
no invented publisher anchor, reference or opinion owner. Original text, source
graphs, 422 existing prose nodes and 19 genuine overlapping paragraph ranges
remain exact; the latter require a separate source-boundary qualification.
Earlier heading/browser receipts retain their actual `d7c21ebf` producer.

One broader pass with `16ad273f` processes **all 40,211 retained witnesses /
89,662 nonempty inputs / 321,688 ordered fields** in **165.34 seconds**. The
paired comparison preserves source/frame/field identities, input and rendered
text hashes, query-text length, source element counts/tags and literal lookup
facts. It finds **zero errors or output-cap omissions**, and no paragraph-count
changes beyond the qualified 18 removals. These compact facts establish the
reported preservation checks, with no full graph-value, node-range or semantic
recall claim. Private proof: `native-note-marker-breadth/comparison.json`.

The `6c901d33` candidate preserves independently paginated reporter tracks.
An explicit `pagescheme` or citation index follows its own next marker rather
than a different reporter's marker. Page lookups materialize their selected
pages; overlapping reporter ranges do not imply parent-child ownership.
The frozen eight-field comparison preserves all **124 Native page markers**
and restores **70 truncated intervals in three fields**, including their
existing label and printed-alias queries. Every unrelated complete output value
remains exact. The **94 empty PAGE@number declarations without a scheme** stay
generic source evidence. No reporter, publisher ID or final-page scope is
invented. Private proof: `native-pagination-tracks/fixed-query/candidate-comparison.json`.

The same executable also qualifies **4,357 source-supported parent repairs**
across 71 fields: 4,006 missing owners and 351 nearer owners. All other complete
record values remain exact, including 381 note-marker overlap controls and
406 Pages. Private proof: `native-declared-ancestry/candidate-comparison.json`.
These receipts bind the compiled source snapshot; a subsequent independent
working-tree change is recorded separately and was not loaded by the executable.

A diverse cached corpus qualifies **303 actual source anchors** across 182
target-bearing pages, including opinion boundaries, headings, notes/references,
paragraph sources and reporter pages. The Rust capture includes 183 pages;
one additional citation destination lacks the requested source ID. The unchanged
single-worker browser runner preserves its delays and observes every selected
target visibly settling in the correct opinion. Source categories remain
separate from native semantic roles. Private proof:
`native-cached-source-links/cached-source-link-comparison.json`.

The `24d3b515` candidate exposes leading printed paragraph markers through
`sourceParagraphMarkers(document)` and
`sourceParagraphMarker(document, markupStart)`. The latter uses the actual
source P opening-byte position, keeping repeated numbers distinct. Each view
retains the existing source element, physical ancestors, overlapping marker
elements, rendered UTF-16 marker/number ranges and any unique literal P ID.
The independently frozen **68-field** comparison observes **1,749 markers**,
**1,748 positive getters and 40 null controls** in **1.12 seconds** of actual
Rust execution. Every old complete record value remains exact. The two isolated
struck markers remain source evidence, with no active-numbering claim. These
views do not assign canonical legal paragraph ends or publisher landing
eligibility. Private proof:
`native-paragraph-marker-query-corpus/candidate-comparison.json`.

The `8d061606` candidate also preserves an explicit star-page marker inside a
source footnote. Its twelve-field complete comparison restores one page and
changes only the four independently declared page/query values; all note, text
and source-graph values stay exact. A separate **307-row / 5,866-cell** routing
comparison preserves complete originals while recognizing flat oral-argument
transcripts and disclosure addenda as text. These bounded results do not replace
whole-source form discovery. Private proofs:
`native-note-prefixed-page/candidate-comparison.json` and
`native-extra-text-routing/candidate-comparison.json`.

The same candidate passes the affected Node Rust binding compilation with
optional PDF support disabled. This compiles the query dispatch without linking
an addon or establishing JavaScript/WASM runtime behavior.

Its complete-record integration comparison also passes **147 observations**
covering pagination, ancestry and printed markers. Independent original-source
review accounts for **150 added star-page declarations** and their exact
304-operation supplement; all other complete values stay exact. Three complete
collection streams have since replayed with the corrected observer: oral
arguments, disclosures and all **10,174,551 clusters**. The cluster inventory now
retains **756,154 form keys / 28,425 complete witnesses**, with zero cap omissions.
These collection results establish discovery and text routing, with Native
legal-semantic qualification still separate.

The headmatter review is a corpus-emission gap, not parser loss. An actual
58-field source run preserves all **956 literal elements and 559 source-ID
queries**, with 187 foreign body/UI membership controls absent. The existing
source query already addresses these elements. Private proof:
`native-headmatter-source-addresses/candidate-comparison.json`.

The subsequent `af76d654` emitter publishes all **559 headmatter targets** with
explicit source-region ownership and preserves all **58 old receipts byte
exactly**. The cached runner observes **559 visible exact landings**, rejects
**187 foreign-region controls**, and preserves **12 existing opinion landings**
in **346.85 seconds** with its original timing. Private proofs live in
`native-headmatter-link-emission-review/`. The same producer passes eight full
heading/appendix records: ten source-derived outline levels, two matched
appendix Sections and ten descendant parent fields. Repeated-title filtering
still needs a separately qualified rank-stack correction. Cached landing and
these finite comparisons do not establish live or whole-corpus completeness.

Public bulk data and Internet Archive mirrors are the acquisition route. The
actual seven-resource mirror capture retains a docket JSON, word-level ASR,
timed VTT and filing hOCR plus their original item catalogs, totaling
**1,754,746 distinct source bytes**. All four selected file identities match
their catalog size, SHA-1 and MD5. The docket has **253 document IDs**, with
**211 unique nonempty document URLs and 42 empty URLs**: preserve IDs,
attachment/sequence relationships, availability and source paths independently.
The hOCR has four distinct pages
whose literal `ppageno` values are all zero; retain page IDs, image references,
geometry and order rather than collapsing them by that field.
`all-collections/mirror-samples/report.json` binds the original entities and GET
receipts. These samples establish available representation shapes, not a full
mirror census or native semantic qualification. Historical unauthenticated API
error receipts supplied no records and do not constrain this public route.
Source binaries remain separate pending coverage. Oral-argument
CSVs include machine transcript fields; audio and disclosure forms remain
separate sources. The latest listed embedding inventory manifest returned
**HTTP 403**; its contents and vectors were not acquired by the catalog capture.

Compile from the Beaver workspace root, using its ordinary Cargo target:

```powershell
$env:CARGO_ENCODED_RUSTFLAGS = "--remap-path-prefix=$((Get-Location).Path)=."
cargo build --manifest-path legal-structure/experiments/courtlistener-corpus/Cargo.toml --release -j 1
```

The executable is `native/legal-structure-node/target/release/courtlistener-corpus.exe`.
No unit or integration suites are part of this session; validation consists of
real corpus runs. Acquisition adapters are outside the engine. Compression,
metadata processing, structure recovery and rendition comparisons use Rust.

Commands:

- `census OUT MAXIMUM_BYTES WITNESS_BYTES FEATURE_LIMIT [ROW_LIMIT]`: consume
  decoded PostgreSQL COPY CSV from stdin, count all columns and source forms
  across the eight renditions, and retain bounded complete-row witnesses in
  seekable Zstandard frames. The optional row limit is for partial runs;
  snapshot completion additionally requires the verified transport receipts.
- `pack OUT [LEVEL BLOCK_BYTES MAX_STORED_BYTES] [--exclude-pack DIRECTORY]...`: read source JSONL from stdin,
  preserve every input byte in independently seekable Zstandard frames, and
  decompress each frame immediately to compare with its actual source. Indexes
  retain source membership, frame offsets, lengths and SHA-256 digests.
  Optional same-snapshot exclusion packs use verified index membership without
  inflating source records; receipts distinguish submitted, stored and skipped rows.
- `metadata PACK SOURCE_DIR OUT BZIP2`: join complete official cluster, docket,
  court and citation CSV snapshots to the source opinion membership. Selected rows retain
  every column and distinguish unquoted NULL from quoted empty strings. The
  original compressed metadata files remain available on disk.
  The joined v2 cache stores each table's `columns` once and complete arrays in
  `rows`, keyed by source ID. `metadata::lookup(joined, table, id)` reconstructs
  one complete field object; a missing ID returns NULL. No column is discarded.
  A derived `cluster-membership.json` per part records source cluster IDs and
  opinion IDs lacking them, bound to the untouched index digest and row count.
  Packing creates it directly; existing parts receive one verified backfill.
  Matching caches avoid repeated opinion inflation; metadata CSV scans remain.
  Pool joins freeze exact invocation-time `source.json` bytes beside `OUT` with
  extension `source.json`; that same parsed snapshot drives membership traversal.
  Joined data and the output receipt bind its SHA-256, bytes, filename, population
  count and per-part paths/index digests. Later acquisition appends are excluded.
  Citation evidence preserves complete decoded rows, original CSV records and
  one-based source record ordinals after the header. Bulk CAP citation order is
  explicitly unknown; these ordinals never generate `pageCitations`.
  Citation `original` arrays use the shared citation `columns` header; consumers
  retain that header with selected row evidence.
  The sibling `OUT` with extension `receipt.json` records actual uncompressed and
  compressed output bytes, output SHA-256 and retained citation counts/provenance.
  Serialization, compression and persisted reconstruction verification stream
  through bounded buffers. Raw and reconstructed byte counts and SHA-256 digests
  must agree; no full serialized or decompressed JSON byte vector is retained.
  Complete selected tables still occupy memory proportional to their population;
  streaming bounds serialization buffers, not the entire process working set.
- `metadata-membership PACK OUT`: export sorted source cluster IDs, explicit
  missing opinion IDs, SHA-bound part/cache provenance and elapsed time. Reuse
  matching caches; backfill only missing ones through the verified pack reader.
  This command does not read metadata CSV files and requires a new output path.
  Its `opinion_source` descriptor identifies the exact loaded pool manifest SHA,
  byte count and population without creating a second corpus or sidecar.
- `metadata-citations JOINED_METADATA SOURCE_DIR OUT BZIP2`: enrich an existing
  frozen join with citation rows for its retained clusters, using the same original
  snapshot provenance. Preserve all existing source, cluster, docket and court
  values. Scan only the citations CSV; require new output and receipt paths.
- `metadata-selected JOINED_METADATA CANDIDATES_JSON OUT`: prepare a small
  complete context once for a cached iteration cohort. Retain selected clusters,
  their referenced dockets, all original court rows and complete ordered citation
  wrappers. Original NULLs, empty strings, columns, CSV bytes and ordinals remain
  exact. `metadata_subset` binds input/candidate hashes and distinguishes the
  original full-source provenance from actual retained subset counts. Require
  new output and receipt paths; subsequent `richness` runs read this context.
  Complete metadata attached to cached candidates is materialized in the input's
  original column order; duplicate rows and related IDs must agree. Use the
  retained `metadata/catalog-fallback-v2.json.zst` for these cached iterations.
  It supplies full headers, all original courts and uncataloged/ID-only control
  rows. Missing citation evidence remains distinct from a known empty array.
  The fallback does not replace population metadata for a new `catalog` build.
- `derive-pack PACK OUT [MAX_STORED_BYTES [plain-text]]`: store retained sources and exact
  aliases/annotation insertion recipes. Reconstruct every original row, then
  reopen the persisted pack and verify the complete reconstructed row digest.
  Compare actual compressed storage against the independent original pack.
  The optional `plain-text` policy retains exact aliases but only uses annotation
  recipes when upstream's source rendition is plain text.
- `verify-pack ORIGINAL CANDIDATE OUT`: reopen both packs, compare complete
  canonical field-value digests, count exact upstream-source reconstructions,
  and measure Rust replay separately from I/O, extraction and hashing.
- `stream PACK`: emit reconstructed full source rows through a buffered native
  JSONL stream. Pipe directly to `pack` to compare compression levels without
  materializing another inflated corpus.
- `stream-selected PACK CANDIDATES_JSON`: emit complete original records for
  selected `opinion_id` values. Read frame IDs from the existing index and inflate
  only matching seekable frames; verify each selected frame's complete bytes and
  membership. Missing or repeated selected IDs fail. This prepares a diverse
  actual-corpus iteration cohort without rescanning the inflated broad sample.
  A pool input still validates every part index. For a small cached selection,
  reuse the candidate's frozen part provenance, invoke this command on each
  selected part with its own candidate subset, and concatenate in frozen
  part/frame/row order. Reverify selected index/frame hashes and complete values;
  this path does not reverify unrelated part indexes.
- `decode FILE`: decompress a Zstandard receipt to stdout with the native
  streaming decoder.
- `catalog PACK OUT JOINED_METADATA`: index and rank diverse distinct-cluster
  browser candidates once. Canonical URLs require the published cluster ID and
  slug; missing metadata cannot produce an ID-only URL. Source features cover
  every populated markup rendition, with per-rendition and per-range provenance.
  Missing canonical metadata is counted explicitly. A `.zst` output uses the
  existing streaming lossless codec to avoid an inflated metadata catalog.
  The invocation freezes a growing pool's exact manifest beside its output and
  classifies that loaded source only; later committed parts belong to subsequent
  catalogs. Its header records both the opinion manifest and metadata digests.
- `select CATALOG OUT COUNT`: select from the ranked index without reading,
  decompressing or classifying the opinion corpus again. Cached catalogs may be
  JSONL or streaming Zstandard JSONL; selection reads only the requested prefix.
- `richness PACK OUT LIMIT JOINED_METADATA [--cap-citations CONTEXT OPENLEGALDATA_HOME] [--retain-rendered-text]`: compare every populated rendition
  individually with the real native engine and independent HTML DOM observations.
  A zero limit includes every retained input record. An alternate rendition gets
  no plain-text fallback from another field. Report exact field equalities,
  source tags/IDs/links, attributes, semantic classes, media, pagination,
  footnote bodies/references, opinion boundaries, headings, lists and tables.
  Compare exact source-witness fingerprints across each record's renditions;
  retain original bytes regardless of a witness's uniqueness or parser result.
  Report native, heuristic and model nodes separately, including supplementation
  ranges and whether any supplementary node claims a source anchor. Source-ID
  retention is measured against the generic source graph; duplicate-ID query
  ambiguity is counted separately from lost source evidence.
  Diagnostics and page/note relationships remain in each parsed receipt.
  `--retain-rendered-text` also records each rendition's actual query text for
  independent range/ownership review in small cohorts. Other renditions' or
  plain-text coordinates cannot establish its rendered ownership.
- `web-run MANIFEST OPENLEGALDATA_HOME OUT [--structure-only] [--cap-citations CONTEXT]`: compare native recovery from saved
  opinion bodies with independently captured browser DOM anchors and text.
  Exercise paragraph lookup and sample random 6–48 word passages across each
  eighth of the independent visible browser text, including unanchored prose,
  notes and crossings between source blocks. The SHA-256 seeds are repeatable.
  Input HTML and browser evidence hashes are checked. Browser evidence is never
  regenerated from parser output; planned fragments are not paint proof. Emit
  the complete native plans, source texts and a manifest pointing to the original
  content-addressed HTML for the existing cached Chrome runner.
  `--structure-only` disables fragment planning. Both modes compare anchored
  paragraphs, blockquotes, headings, source tags and parent relationships against
  the retained HTML and independent browser evidence. Explicit note links are
  compared by their actual targets, reference IDs and rendered marker text.
  `anchor-targets.jsonl` records exact source-ID targets and published
  opinion-heading links separately from legal-label queries. Whole-page ID
  uniqueness and captured visibility determine eligibility. These are
  source/locator checks; the cached browser adapter supplies navigation evidence.
  Receipts also retain the complete native graph, including unanchored source
  paragraphs, headings, roles, lists and tables. `markup_start` is the opening
  tag's UTF-8 byte position in the exact markup input; node ranges remain rendered
  UTF-16. `markup_attributes` retains decoded attribute values. Unanchored source
  identities support the graph and do not become browser IDs. Existing table
  queries return cells with validated coordinates and spans.
  Optional CAP citation context verifies complete retained CAP JSON, its original
  citation array and official archive-path identity witnesses before supplying the
  existing `pageCitations` input. Web captures additionally require the exact
  retained served-HTML SHA witness. Display order and bulk CSV categories never
  establish reporter-index order. Unknown bindings keep empty labels. Receipts
  bind provided labels to their context and CAP digests; qualified page aliases
  and their range-query outcomes are recorded separately from browser navigation.
- `source-run MANIFEST_JSONL OPENLEGALDATA_HOME OUT`: parse exact retained CAP
  casebody XML, rich HTML and declared plain text. Entries identify their original
  SHA-256, `corpus_path`, source URL and `format` (`cap-json`, `html` or
  `plain_text`). Plain text uses the existing text-only input, has no markup
  region/serialization and is counted separately from markup bytes. Keep complete CAP
  JSON, including citation order and metadata; extracting its XML is a parser
  input, not an archival replacement. These source IDs do not establish the
  deployed CourtListener IDs. Pass the retained citations in the existing
  CourtListener consumer's stable type order and record page-alias query results.
  Compare these with original citation metadata and independent XML page markers;
  emitted aliases alone are not an independent correctness oracle.
  Receipts include native page/note lookup outcomes and document outlines.
  A separate `heuristic_pinpoint_queries` collection exercises the same public
  range-query path for inferred pages and footnotes, including returned text.
  The native-only query collection keeps its original scope. Compare inferred
  results with independently frozen source roles and boundaries; a successful
  lookup alone does not establish correct ownership.
  `source_note_label` resolves an actual ID on a printed note label separately
  from its body anchor; it does not invent a linked reference. Page alias outcomes
  match the original source marker and exact returned UTF-16 range.
  Native heading display uses printed text while source labels and anchors remain
  unchanged. Source-preservation and query/outline comparisons are separate gates.
  An HTML entry with `source_region: "headmatter"` parses that exact served
  container as an excerpt, keeping its byte positions in the complete original
  page. Missing requested containers are counted explicitly. Headmatter and each
  opinion body remain separate inputs so sibling opinions cannot merge.
  Entries with `source_paragraph_marker_requests` add the printed-marker
  collection and getter outcomes for actual source P opening-byte positions.
  The two query receipt keys are absent when unrequested. These source views
  preserve annotations and repeated numbers without assigning paragraph ends.
- `run PACK OUT [LIMIT]`: native preferred-rendition timing and query planning.
  This diagnostic does not establish alternate-rendition richness or live link
  correctness. Missing canonical metadata yields no document URL.

`source-run` and `web-run` also record `source_opinion_components`, obtained
through `DocumentQuery::source_opinion_components`. The native/shared operation
is `sourceOpinionComponents(document)`. It returns the original source-declared
opinion nodes in source order with their actual author nodes, literal attributes,
parent opinion IDs, markup hash and external ancestor context. Multiple authors
do not split an opinion. `source_anchor` requires a unique source-local ID;
synthetic node IDs and external ancestors do not acquire browser fragments or
fabricated ranges. Whole served-page uniqueness and navigation remain separate.

The expanded storage limit is **45,000,000,000 bytes total**, not a record-count
target. User-authorized pruning removed **498 files / 16,477,486,291 bytes**:
491 bulk pack bodies containing 3,300,749 unselected opinion rows, and seven
files initially classified as superseded derived data. All 1,091 directly bound
current inputs and all 491 source indexes passed exact hash checks. Selected
complete records and renditions, the
24,302-opinion storage sample, cached HTML/DOM, independent gold and failures,
raw metadata, and the latest expanded join/catalog remained available at that
pruning checkpoint. The later join retirement is described below. Private
proof: `corpus-pruning/pruning-receipt.json` in the owning rendition benchmarks.

That direct inventory missed a transitive CAP citation-validator dependency:
the deleted 111,511,608-byte selection catalog. Compact recovery preserves all
737 required complete catalog row values with their original canonical hashes,
using 3,707,940 bytes of witnesses and a 5,038,903-byte derived citation context.
Only catalog path, row ordinals and file descriptors change; original contexts,
gold and failures remain. Use the derived
`metadata/catalog-witness-recovery/full-citation-context.json` for future CAP
runs. A real Rust corpus run verified all 737 CAP originals and complete catalog
row witnesses, with three retained served-page citation arrays unchanged and
zero failures. This confirms source identity and citation-context restoration;
it does not accept the rejected heading candidate. The smaller five-case
context also admitted the continued current corpus capture. Private proofs:
`metadata/catalog-witness-recovery/full-context-recovery-receipt.json` and
`metadata/catalog-witness-recovery/retained-three-run.json`.

The later Rust metadata materialization qualifies **5,000 complete cached
candidates and 28 ID-only controls** against their retained original contexts,
with exact table bytes, ordered columns/keys, NULL/empty values and complete
ordered citation wrappers. All three actual commands pass in **1.62 seconds**
total. The **214,669-byte fallback** preserves full source provenance, all 3,361
courts and all **250 join-only cluster exceptions** with their dependencies and
citations. Following that comparison, the redundant **988,011,987-byte generated
join** is retired. The full ranked catalog, all four original metadata CSVs,
selected originals, gold and previous receipts remain. Private proof:
`corpus-pruning/catalog-context-retirement/retirement.json`. The metadata-only
build `46d75111` preserves the qualified parser/query source hashes; earlier
structure and browser captures keep their actual `d7c21ebf` producer identity.

The 491-window acquisition manifests now describe historical acquisition;
their retired bodies are unavailable for `stream`, `stream-selected`, `catalog`
or parser runs. Reuse the preserved selected originals and remaining packs.
Pruning does not trigger a download or restart bulk acquisition. Lower node
counts, parser errors or absence from today's preference order do not establish
rendition redundancy. No comparison command deletes a rendition.

The served-page minimum is **7,000 distinct opinion HTMLs**, adding 2,000 to the
original 5,000-page batch. The copied collector continues through cached diverse
candidates under the shared cap; publisher browser delays and behavior remain
unchanged.

For ordinary-anchor navigation on the complete saved pages, use
[`verify-cached-anchors.py`](../../../experiments/courtlistener-ucd/verify-cached-anchors.py)
with the output directory of `web-run`:

```powershell
python -u -X utf8 experiments/courtlistener-ucd/verify-cached-anchors.py `
  benchmarks/local-data/courtlistener-renditions/web-expanded-structure --workers 6
```

This adapter reuses the existing cached Chrome runner and performs no publisher
requests. It checks actual `:target`, source tag, opinion membership, visibility
and viewport landing after browser navigation. Cached HTML landing is separate
from live-site behavior. Keep each run's frozen input manifest and native source
hashes with its receipts; acquisition can continue without changing that cohort.

For independent structure evidence, run the same cached adapter with
`--structure-gold`. Compare that saved browser inventory with a native run:

```powershell
node experiments/courtlistener-ucd/compare-source-structure.mjs `
  benchmarks/local-data/courtlistener-renditions/web-source-structure `
  benchmarks/local-data/courtlistener-renditions/web-expanded-structure/browser-structure-gold.jsonl
```

The comparator checks the selected source tags, decoded attributes, visible
text, parent relationships, anchors and table-query results. Both inputs retain their original HTML
hashes. It records `textContent` separately from visible text and never creates
gold from the parser's output. The receipts are real corpus runs, not fixtures.

Use `--all-elements --source-context --styles STYLESHEET_CAPTURE_JSONL` to inventory
every opinion element, its wrapper and the actual ancestor attributes separately.
The cached adapter verifies the retained publisher CSS digests and applies each
stylesheet in its original page order. It blocks external scripts, images and
fonts; those restrictions remain part of the measured scope. For navigation it
waits for natural target/viewport scrolling to settle, without manually scrolling
or disabling publisher smooth scrolling. The original UCD acquisition delays
remain unchanged.

`web-run` slices opinion markup directly from the original serialized browser
HTML. Receipt `source_markup_byte_start`/`source_markup_byte_end` locate that
exact slice in the original complete page. An element's `markup_start`/`markup_end`
locate its original bytes within that slice; rendered ranges are UTF-16. HTML5
reparsing can discard a leading PRE newline and repair nested anchors. The
independent literal-source and XML inventories record source nesting separately
from browser reparsing. `reconcile-source-oracles.mjs` explains these observed
differences against original live-acquisition text and independently retained
source evidence; it does not silently waive unknown gaps.

The engine keeps every CourtListener `MarkupElement` in source order, including
unknown roles and inline wrappers. Attributes, direct parents, source-byte
spans, original-input SHA-256 and ancestor context remain distinct from projected
legal nodes. `DocumentQuery::source_element` resolves a nonempty, case-sensitive
ID; duplicates are ambiguous. The generic graph supports future interpretation
without inventing legal roles or browser fragments. Original comments, text-node
bytes, media and complete record metadata remain in the retained sources.

The new `DocumentQuery::source_markup` returns a borrowed `SourceMarkup` view of
that complete graph, its original-input hash and ancestor context; the
native/shared operation is `sourceMarkup(document)`, exposed in consumer source
and its typed contract. The compiled CLI passes an independent **14-field
retained-source comparison**, exposing every existing element, ancestor and
source hash while preserving every complete prior record exactly. The affected
native dispatcher/engine check passes with PDF disabled. Addon linking and
Node/WASI/browser runtime outcomes remain unmeasured; this finite graph proof
does not establish whole-corpus legal semantics.

## Acceptance gates

The current priority is **breadth of native structure recovery and useful
supplementation across CourtListener**, following the maintainer's scope
clarification. Keep the measured compressed archive and retained originals as
the working baseline. Further storage encoding work is secondary unless a
concrete size or speed advantage emerges. Compare source families and renditions,
including headmatter, separate opinions and plain-text records; report observed
source structure and heuristic additions separately. Supplementation must use
the engine's existing inference and provenance model, preserve native facts,
and avoid turning inferred legal labels into fabricated publisher anchors.

Prioritize opinion components and authorship, heading hierarchy, paragraph
boundaries and numbering, notes and their references, pagination and citation
schemes, citations and source anchors across every available rendition. Tables
and embedded content retain their source evidence and existing capabilities,
with lower priority for further semantic work. The measured plain-text storage
thesis is closed: derivation supplied no additional compressed saving. Retain
complete compressed renditions as the archive baseline. Plain-text
supplementation is one recovery path within this broader work.

**Whole-source discovery is complete for the pinned 2026-09-30 opinions
snapshot.** The terminal `full-snapshot-v2` receipts establish all **10,903,928
source rows / 22 columns**, not a sampled byte-position union. The census tracks
**128,182 normalized rendition/token forms**, with no overflow, omitted feature
inventory or missing retained exemplar, and **40,211 complete original-row
witnesses**. This completes source transport and the declared discovery
projection; legal interpretation, exact lexical/namespace qualification, parser
recovery and browser navigation remain separate gates.

The earlier catalog of **438 windows / 2,958,294 records**, historical acquisition
of **491 windows / 3,300,749 records** and **29.818% compressed-position union**
remain historical partial-sample measurements. Their **53 uncataloged windows /
342,455 records** no longer limit the new whole-source census. The pruned bulk
bodies remain retired; the stream retained bounded witnesses, not a replacement
archive, and does not authorize bulk refill.

The next work is independent retained-source qualification of the full
inventory's opinion/byline families, LDML ownership and heading depths,
paragraph numbering, note/reference groups, page/citation declarations and
source addresses. Full counts and later rare forms are ranked in
[the findings](FINDINGS.md#full-snapshot-inventory-and-next-structures). The
728-input comparison remains a preservation gate for known forms; its legal
results do not extend automatically to the newly inventoried families.

The implemented census passed a real **5,000-row retained-source qualification**:
15,249 feature keys and 1,096 complete witnesses were reopened exactly, with
original column values and presence counts preserved. Native time was
**19.295 seconds / 22.305 seconds total**. Its COPY CSV is derived from retained
values, not the remote archive's original CSV bytes. Private receipt:
`whole-corpus-census/retained-5000/qualification.json`.

The first full-source attempt **failed and is incomplete**. It observed
**851,916 rows** and read one further row before the source feature-key ceiling
would be exceeded at zero-based ordinal **851,916**. The retained inventory has
**499,999 feature keys** and **31,149 complete witnesses**, with no missing
exemplars or witness saturation. Transport stopped after **3,233,058,816** of
**55,252,643,429 compressed bytes**; CSV EOF was not reached. The failed prefix
and its receipts remain.

The key explosion is traced to **468,110 per-instance identifier keys** and
**20,231 party-name atoms**, together **97.67%** of retained keys. Exact source
values remain in the private witnesses. The normalized revision groups long
mixed-alphanumeric IDs and party-name values into forms, while retaining
structural literals and exact ID/href matching. It also continues exact row and
column counts after a feature-key ceiling, reports overflow by rendition and
marks `feature_inventory_complete` false.

The corrected census passed a replay of **all 31,149 retained prefix frames**,
reopened exactly. It tracked **37,434 forms** with **12,711 complete output
witnesses** and no overflow, missing exemplars, omissions or saturation:
**222.827 seconds Native / 244.081 seconds total**. A separate real 5,000-row
run with a **100-key limit** retained exact counts and reopened all 5,000 complete
values while explicitly reporting incomplete feature inventory and per-rendition
overflow. Private proofs: `whole-corpus-census/retained-prefix-witnesses-normalized/qualification.json`
and `whole-corpus-census/retained-5000-overflow/qualification.json`.

The second full-source attempt, `full-snapshot-v2`, **completed on 2026-10-07**
from the same pinned object with no row limit. Curl, bzip2 and Native all exited
zero; the pinned ETag/content length, complete **55,252,643,429 compressed
bytes**, **353,863,767,610 decoded bytes**, decoded SHA-256 and CSV EOF agree
between terminal transport and Native receipts. Total elapsed time was
**24,043.99 seconds (6 h 40 m 43.99 s)**. Witness data occupy **412,257,242
compressed bytes**, with a **28,547,053-byte index**; capture did not saturate
and omitted no requested witness rows. These are source rows, not a separately
verified global count of distinct opinion IDs.

The scan used the qualified `1bd48e9e` producer frozen in the canonical release
directory, allowing ordinary CLI rebuilds during transport. The owning parent
retires this process-lifetime copy after its children close. The wrapper requires
an admitted `COURTLISTENER_CENSUS_ALLOCATION`; the completed invocation was:

```powershell
node experiments/courtlistener-renditions/census.mjs `
  benchmarks/local-data/courtlistener-renditions/broad/source.json `
  native/legal-structure-node/target/release/courtlistener-corpus-whole-source.exe `
  "C:/Program Files/Git/usr/bin/bzip2.exe" `
  benchmarks/local-data/courtlistener-renditions/whole-corpus-census/full-snapshot-v2 `
  4294967296 2147483648 500000
```

Regeneration requires a new allocated output owner; preserve the existing run
and receipts, including the failed `full-snapshot` attempt.
`transport-progress.json` and `progress.json` retain progress
observations; a last running observation does not supersede a terminal failure.
The terminal `transport-proof.json` plus `summary.json` establish full
source completion: matching pinned HTTP identity, complete compressed bytes,
successful curl/bzip/Native exits, exact decoded-stream hashes and CSV EOF.
Source ordinals count rows, not proven distinct opinion IDs. The confirmed
[official COPY CSV contract](https://wiki.free.law/c/courtlistener/help/api/bulk-data/bulk-legal-data)
uses backslash escaping and distinguishes NULL from quoted empty strings.

Native output is bounded to **4 GiB**, with a **2 GiB complete-witness subset**
and **500,000 feature-key ceiling**. The first producer failed at the feature
ceiling; the qualified revision separates incomplete feature inventory from
all-row source counts. The total inventory output bound remains a hard failure.
Witness saturation continues counts and reports omitted rows and features
lacking exemplars. Source completion, feature-inventory coverage, representative
coverage, legal-role recovery and publisher navigation are separate claims.
HTML5 DOM repairs, normalized value forms and lexical ownership cues are
discovery projections, not independent legal-role gold or navigation proof.
Qualification producer and stable locked-build hashes are in
`whole-corpus-census/build-locked.json`. The normalized build records an unrelated
inference source change during compilation; its
`whole-corpus-census/build-normalized-census-scope.json` proves stability only for
the census, extractor, CSV and helper sources. It makes no core-semantic claim.

The historical prefix prioritized `case`/`opinionby`/`opiniontype` authorship forms,
`anchor`/`idref` without their own href, normalized citation fields, zero-text
`PAGE.number` and `H.level`. The first two now have a finite accepted comparison:
**eight complete records** match frozen source expectations, adding **five
declared byline associations** and **28 explicit idref markers** across **14
existing Notes**. All prior references, note bodies, paragraphs, source graphs
and other queries remain exact. Candidate time is **0.592 seconds**, against
**0.624 seconds before**. This accepts the immutable `8a0a1c22` producer and
bound query/native modules, not the externally changed current source tree or
whole parser. Private score: `declared-author-reference-breadth/candidate-score.v3.json`.

The full inventory now adds Columbia `opinion_byline`/`opinion_text`, concurrence
and dissent pairs, LDML author/type/note/appendix scopes, rare `fnpara1`/`fnpara2`
and headnote-group forms to that queue. Hierarchy, normalized citation roles,
zero-text page semantics, namespace resolution and further ownership/reference
branches still need source-backed qualification. Rendered UTF-16 coordinates
in the accepted eight-record result use verified before graphs and printed
glyphs, not independent rendering gold. Publisher navigation remains separate.

The **eight-record page-declaration** and **six-record citation-declaration**
cohorts now pass the compiled full-graph query candidate with **zero diagnostics
/ 5,794 independent source correspondences**: 3,865 page-cohort elements and
1,929 citation-cohort elements. Every old complete record remains exact; the
sole addition is `source_markup` containing the existing graph, ancestor context
and original-input hash. Sixty-two citation attributes qualify through observed
one-pass entity decoding; raw gold and the initial literal-attribute failure
remain retained. This qualifies the immutable `38c592e3` CLI and its bound source
identity, adding no new page/citation legal roles or navigation proof. Private
scores: `source-pagination-declarations/candidate-comparison.json` and
`source-citation-declarations/candidate-comparison.json`; full hashes, build/check
scope and timing are recorded in [the findings](FINDINGS.md).

The **eight-field Columbia candidate passes** its frozen independent contract:
all eight complete candidate records and eight component queries are exact,
with **zero diagnostics**. It adds only eight source-supported unowned bylines
in source opening order, with unknown author and anchor, preserving the four
existing opinion/author associations and every other complete record value.
Of eight body declarations, only two closes match their own role; **six crossing
body scopes remain unresolved**, with literal closing tokens separate from parsed
extents. No sibling ownership or new body sections are established. This accepts
the immutable `ec540a51` producer for this cohort. Private score:
`columbia-opinion-components/candidate-comparison.json`; exact identity and
measured timing are recorded in [the findings](FINDINGS.md).

The formatted-heading comparison has a frozen source-only contract for
**269 additions in 54 of 728 records**, including 12 logical headings split
across marker/title paragraphs. It requires exact preservation of the other
674 complete records, original paragraph nodes, opinion components, source
graphs and existing queries. Formatting candidates are not role gold; court
names, dates, dispositions, authorship and emphasized note prose supply negative
controls. The contract precedes candidate observation and does not certify a
parser result. The first actual candidate was rejected for 96 additional or
missing heading/outline diagnostics; prior source graphs and existing queries
remain exact. Its first revision was also rejected, with five diagnostics in
four records. The latest inference source (`9af69483`) has no heading-contract
corpus result; the subsequently compiled census CLI does not certify its roles.
No heading candidate is accepted. Private contract:
`source-formatted-headings/comparison-contract.json`; revision result:
`source-formatted-headings/comparison728.revision-failed.json`.

The active objective is preservation and recovery across arbitrary CourtListener
records in the independent Rust engine. Whole-source discovery for this pinned
snapshot is complete; finite comparisons still constrain recovery within their
measured scope and do not certify other source families. Completion requires all
of the following:

1. **Completed discovery gate:** the pinned whole-source stream and declared
   normalized DOM/form census, with exact row/column coverage, zero overflow or
   omissions and a complete original witness for every tracked form. This does
   not complete lexical, namespace or legal-role support classification; those
   comparisons remain below. Selected validation inputs are not the census
   population.
2. At least 7,000 distinct, valid served opinion pages, acquired through the exact
   copied UCD pathway within the shared 45 GB cap. Record actual marginal coverage
   across jurisdiction/court, era, source/rendition, status, opinion/authorship,
   OCR, size, structural features and provisional subjects; keep selection and
   successful acquisition counts separate.
3. Frozen, hashed original renditions, complete joined metadata, CAP sources,
   original browser evidence and required rendering/media evidence. Preserve
   full bytes and explicit NULL/empty values until a representation is proven
   reconstructible. Sparse native output is never evidence of redundancy.
4. Independent comparisons covering every observed source element and attribute,
   order/parents, namespaces, source addresses, declared roles, boundaries,
   notes/references, citations, tables/spans, pagination/aliases and geometry.
   Explain every measured discrepancy or retain it as an unresolved gate. Keep
   source facts separate from inferred legal meaning.
5. Source-address queries that preserve case, ambiguity, point/image anchors,
   genuine reference targets and whole-page identities. Associate each emitted
   URL with the actual canonical cluster/slug and original source anchor.
6. Measured browser outcomes by link type, with natural navigation and independent
   target/body/visibility/landing observations. Quantify rejected and unverified
   cases. Cached outcomes and representative live behavior are separate gates;
   text-fragment paint evidence is separate from ordinary-anchor evidence.
7. Exact full-field round trips for every admitted storage derivation, with an
   original retained fallback for unknown transforms. Report raw versus
   compressed savings, additional recipe savings and query replay separately.
   Do not adopt a smaller projection as the archive merely because queries work.
8. A reproducible preservation contract, byte accounting and source/code hashes,
   zero unexplained losses in the declared measured scope, explicit remaining
   uncertainty, and the final capability implemented in the independent Rust
   engine. Validation in this session is real corpus runs and necessary
   compilation only; no tests are written or run.

Source-position samples remain partial: they include complete recoverable CSV
records inside named compressed windows and can omit large boundary-crossing
records. Coverage must report actual courts, eras, subjects and source features;
these samples do not prove behavior across arbitrary CourtListener records.

## Established history

Read the existing findings before interpreting new receipts:

- Beaver `c35d45e3` surveyed provider richness on 2026-07-29. CAP supplies
  pagination, footnotes, opinion boundaries, semantic paragraph classes,
  `data-blocks` page/bounding-box witnesses and citation links. These were known
  source facts, not discoveries of this experiment.
- Beaver `f96adec002` recovered CAP page-label anchors and footnote asides.
  `a675aa4c4c` added CourtListener page aliases, citation-index/page-scheme
  handling, symbol footnotes and native lookup. Citation links and footnote
  references were deliberately distinguished from containers.
- Beaver `b3ce2deac0` added a corpus-wide lookup/round-trip audit of the
  preferred rendition. `81dfbdb75c` already supplied a stratified sampling
  catalog; selection should read an index rather than repeatedly scan markup.
- The [retained cutover result](../../../docs/decisions/source-structure-cutover-results.md)
  records 55,504 exact CourtListener matches on 2026-08-20. The
  [earlier diagnostic](../../../backend/experiments/source-structure-parity/COURTLISTENER_RESULTS.md)
  isolated numeric-only newline handling, not a need for lexical exclusions.
  These prove the historical preferred-rendition contract, not completeness of
  all renditions or browser link landing.
- The standalone engine owns rendering, structure and query planning. Its
  `917615b` change already reuses the text-fragment replay index within a
  document query; consumers should reuse that query instead of rebuilding it.

The established richness findings direct the storage and parser work. Trace
which upstream transformations actually produce each rendition, compare exact
reconstruction and measured storage, preserve the known witnesses and arbitrary
source anchors, and measure links against the actual served opinion. Parser
counts alone cannot justify deleting source evidence.

## Forward reconstruction

The pinned upstream revision is `79cdb523158861eeabb19f2aea296529d007b992`.
[Source selection](https://github.com/freelawproject/courtlistener/blob/79cdb523158861eeabb19f2aea296529d007b992/cl/citations/utils.py)
prefers `xml_harvard`, `html_anon_2020`, `html_columbia`, `html_lawbox`, `html`,
then `plain_text`. This differs from the preferred display rendition.
[Annotation](https://github.com/freelawproject/courtlistener/blob/79cdb523158861eeabb19f2aea296529d007b992/cl/citations/annotate_citations.py)
uses existing markup when available. For plain text it HTML-escapes the source
and inserts citation and `pre` wrappers. Citation resolution also supplies
targets, case descriptions and spans: plain text alone is not those inputs.

The Rust engine supplies `courtlistener_markup_recipe` and
`restore_courtlistener_markup`. A recipe retains every inserted byte and its
position over the retained source (escaped once for plain text). Query replay
requires no HTML parsing, citation extraction or online resolution. Admission
requires exact equality with the separately supplied annotated HTML; unknown
transformations retain their original value. The experiment measures whether
this representation actually beats ordinary Zstandard compression.

Measured outcomes and remaining evidence live in [FINDINGS.md](FINDINGS.md).

`DocumentQuery::source_list_item(document, native_list_id, ordinal)` queries a
direct source LI within one existing Native OL. Use `document.query_range(node)`
on the returned node to read its rendered UTF-16 span. The query reuses one
document index and retains the original list/item structure and attributes.
Duplicate ordinals, unsupported wrapped ownership and invalid integer attributes
remain unresolved. Source counter ordinals are distinct from CSS marker rendering
and publisher fragments; the API creates no paragraph label or anchor.

The corpus runner accepts explicit `source_list_item_requests` pairs in selected
source manifest records and records actual selected nodes, spans and trimmed text.
Expected counters and owner relationships come from the independent original-source
review. Reversed lists, value resets, integer overflow and same-owner duplicates
remain outside the current observed validation scope.

Native Word-footnote supplementation uses printed labels, actual note bodies
and source ownership rather than Word section names, paragraph classes or
separator layout. A unique source `ftnN` body with a leading numeric Mso or SUP
label receives separate printed-label and body ranges. A unique preceding Mso
marker in the same owner supplies its anonymous reference range; labels can be
recovered independently of references. Conflicting identities and ambiguous
destinations remain unresolved. The body starts at the printed label's end,
retaining separator whitespace under the existing Note contract. The measured
606-record comparison preserves every other node, source element, attribute,
parent, text value and prior query. These anonymous references have no source
anchor; publisher navigation requires separately observed destinations.
