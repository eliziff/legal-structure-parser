# Case-reading source fixture

`1946-scc-tremblay.txt` is the English A2AJ text of *Tremblay v. Beaumont*,
[1946] SCR 448, from the [Supreme Court of Canada's published decision](https://decisions.scc-csc.ca/scc-csc/scc-csc/en/item/8306/index.do).
The reasons are unnumbered. Their three called reporter footnotes are printed
again in the flattened provider text; they establish no numbered paragraph spine.
The test checks the three labels and each one's called body while retaining the
source text and its repeated printings.

UTF-8 source SHA-256: `a0c363c80cc329dd8a8415315c2b0e7ba795138855c0b6f76e39ffc78a03765f`.
It was read from the canonical A2AJ store with the existing `a2ajReads` API,
document 188497, cases, English. Regenerate from that public source only after
review; do not use a parser prediction as the expected source reading.

`2003-fc-993.txt` is the English A2AJ text of 2003 FC 993, from the
[Federal Court's published decision](https://decisions.fct-cf.gc.ca/fc-cf/decisions/en/item/40649/index.do).
Its five called footnotes are numbered 1–5 after the unnumbered reasons; the
test requires notes, including through mixed-style paragraph recovery.
Document ID 93733. UTF-8 SHA-256:
`1de0c958f41c41112facaa50c668980c8e00ef4269a51e244e87dc8bc3455082`.

`2024-onca-71.txt` is the English A2AJ text of 2024 ONCA 71, from the
[Ontario Court of Appeal's published decision](https://coadecisions.ontariocourts.ca/coa/coa/en/item/22053/index.do).
The six numbered reasons start after the publication restrictions and case
metadata. The metadata calls footnote 1, whose body follows paragraph 6.
The first paragraph is a different body bearing the same number; it remains
paragraph 1. Document ID 156341. UTF-8 SHA-256:
`680e8b1ca72df925151e95c1cc78130c9eb3f82acbe35ec833d1a5551f1b048b`.
Both fixtures were read from the canonical store using `a2ajReads`.
