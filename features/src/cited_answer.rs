//! Presentation of authored text with deterministic, already-planned citations.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct CitedBlock {
    pub text: String,
    pub citations: Vec<String>,
}
pub fn render_cited_blocks(blocks: &[CitedBlock]) -> String {
    let mut out = String::new();
    for (index, block) in blocks.iter().enumerate() {
        let table = block.text.starts_with('|') && block.text.ends_with('|');
        if index > 0 {
            out += if table && blocks[index - 1].text.ends_with('|') { "\n" } else { "\n\n" };
        }
        let links = block.citations.join(" ");
        if table {
            let cell = block.text[..block.text.len() - 1].trim_end_matches(legal_structure::javascript_whitespace);
            out += &format!("{cell} {} |", links.replace('|', "\\|"));
        } else if links.is_empty() {
            out += &block.text;
        } else {
            out += &format!("{} {links}", block.text);
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
