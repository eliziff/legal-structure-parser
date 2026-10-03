# legal-structure-parser agent guide

## Publishable data

- Use independently invented fixtures or document their public source. Do not copy
  genuine user queries, bug-report identifiers, histories or private documents
  into tests or evals without explicit permission. Synthetic replacements change
  identifying URLs and locators too.
- Automated prompts carry `machine_test` and a run ID; absent legacy origin remains
  `unknown`. Submission origin and fixture provenance are separate facts.
- Use relative paths or runtime input configuration and GitHub noreply attribution.
  Keep private inputs, auth and raw receipts in ignored local storage. Preserve
  third-party licenses and public-source attribution.
- Rust builders should remap source paths with `--remap-path-prefix`.
