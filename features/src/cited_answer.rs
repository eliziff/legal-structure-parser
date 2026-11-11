//! Presentation of authored text with deterministic, already-planned citations.
use serde::{Deserialize, Serialize};
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
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authored_prose_and_table_cells_are_preserved() {
        assert_eq!(render_cited_blocks(&[CitedBlock{text:"A sentence.".into(),citations:vec!["[Case](https://example.invalid/#par2)".into()]},CitedBlock{text:"| Cell |".into(),citations:vec!["[Case](https://example.invalid/a|b)".into()]}]),"A sentence. [Case](https://example.invalid/#par2)\n\n| Cell [Case](https://example.invalid/a\\|b) |");
    }
}
