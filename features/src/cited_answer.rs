//! Presentation of authored text with deterministic, already-planned citations.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CitedBlock {
    pub text: String,
    pub citations: Vec<String>,
}
pub fn render_cited_blocks(blocks: &[CitedBlock]) -> String {
    let mut out = String::new();
    for (index, block) in blocks.iter().enumerate() {
        let table = block.text.starts_with('|') && block.text.ends_with('|');
        if index > 0 {
            out.push_str(if table && blocks[index - 1].text.ends_with('|') {
                "\n"
            } else {
                "\n\n"
            });
        }
        let links = block.citations.join(" ");
        if table {
            out.push_str(
                block.text[..block.text.len() - 1]
                    .trim_end_matches(legal_structure::javascript_whitespace),
            );
            out.push(' ');
            out.push_str(&links.replace('|', "\\|"));
            out.push_str(" |");
        } else {
            out.push_str(&block.text);
            if !links.is_empty() {
                out.push(' ');
                out.push_str(&links);
            }
        }
    }
    out
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CitationLink {
    pub label: String,
    pub url: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReferenceBlock {
    pub text: String,
    pub citations: Vec<CitationLink>,
}
pub fn render_reference_blocks(blocks: &[ReferenceBlock]) -> String {
    let mut indexes = HashMap::<String, usize>::new();
    let mut urls = Vec::new();
    let mut rendered = Vec::new();
    for block in blocks {
        let mut citations = Vec::new();
        for citation in &block.citations {
            let id = *indexes.entry(citation.url.clone()).or_insert_with(|| {
                urls.push(citation.url.clone());
                urls.len()
            });
            citations.push(format!("[{}][cite-{id}]", citation.label));
        }
        rendered.push(CitedBlock {
            text: block.text.clone(),
            citations,
        });
    }
    let mut out = render_cited_blocks(&rendered);
    if !urls.is_empty() {
        out.push_str("\n\n");
        out.push_str(
            &urls
                .iter()
                .enumerate()
                .map(|(index, url)| format!("[cite-{}]: {url}", index + 1))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authored_prose_and_table_cells_are_preserved() {
        assert_eq!(render_cited_blocks(&[CitedBlock{text:"A sentence.".into(),citations:vec!["[Case](https://example.invalid/#par2)".into()]},CitedBlock{text:"| Cell |".into(),citations:vec!["[Case](https://example.invalid/a|b)".into()]}]),"A sentence. [Case](https://example.invalid/#par2)\n\n| Cell [Case](https://example.invalid/a\\|b) |");
    }
    #[test]
    fn references_keep_exact_targets() {
        let url = "https://example.invalid/case#:~:text=exact%20words,end(a)";
        let block = ReferenceBlock {
            text: "An unchanged sentence.".into(),
            citations: vec![CitationLink {
                label: "Case at para 2".into(),
                url: url.into(),
            }],
        };
        assert_eq!(
            render_reference_blocks(&[block]),
            format!("An unchanged sentence. [Case at para 2][cite-1]\n\n[cite-1]: {url}")
        );
    }
}
