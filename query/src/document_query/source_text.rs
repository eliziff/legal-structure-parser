use super::*;
use unicode_normalization::UnicodeNormalization;

#[derive(Serialize)]
pub struct TextStatistics {
    pub occurrences: usize,
    pub words: usize,
}

fn normalized_fragment_text(value: &str) -> String {
    static WORDS: OnceLock<Regex> = OnceLock::new();
    let normalized = value.nfkd().collect::<String>().to_lowercase();
    regex(r"[\p{L}\p{N}]+", &WORDS)
        .find_iter(&normalized)
        .map(|word| word.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

impl DocumentQuery {
    /// Phrase counts and word counts used to choose a browser fragment, with the source kept here.
    pub fn text_statistics(
        &self,
        document: &DocumentStructure,
        texts: &[String],
    ) -> Vec<TextStatistics> {
        let source = self
            .normalized_fragment_text
            .get_or_init(|| format!(" {} ", normalized_fragment_text(document.query_text())));
        let mut counts = HashMap::new();
        texts
            .iter()
            .map(|text| {
                let normalized = normalized_fragment_text(text);
                let words = if normalized.is_empty() {
                    0
                } else {
                    normalized.bytes().filter(|byte| *byte == b' ').count() + 1
                };
                let occurrences = *counts.entry(normalized).or_insert_with_key(|normalized| {
                    if normalized.is_empty() {
                        return 0;
                    }
                    let needle = format!(" {normalized} ");
                    let (mut count, mut from) = (0, 0);
                    while let Some(at) = source[from..].find(&needle) {
                        count += 1;
                        from += at + 1;
                    }
                    count
                });
                TextStatistics { occurrences, words }
            })
            .collect()
    }
}
