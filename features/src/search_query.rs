//! Beaver's shared CanLII-style query grammar and index dialects.
use legal_structure::{javascript_whitespace as space, trim_javascript_whitespace as trim};
use serde::{Deserialize, Serialize};
const NEVER: &str = "__beaver_no_search_match__";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SearchNode {
    Term { value: String, prefix: bool },
    And { left: Box<Self>, right: Box<Self> },
    Or { left: Box<Self>, right: Box<Self> },
    Not { left: Box<Self>, right: Box<Self> },
}
impl SearchNode {
    fn terms(&self, out: &mut Vec<String>) {
        match self {
            Self::Term { value, .. } => out.push(value.clone()),
            Self::Not { left, .. } => left.terms(out),
            Self::And { left, right } | Self::Or { left, right } => {
                left.terms(out);
                right.terms(out);
            }
        }
    }
    fn compile(&self, lucene: bool) -> String {
        let (operator, left, right) = match self {
            Self::Term { value, prefix } => return match (lucene, *prefix) {
                (false, _) => format!("\"{}\"{}", value.replace('"', "\"\""), if *prefix { "*" } else { "" }),
                (true, false) => format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\"")),
                (true, true) => value.replace('\\', "\\\\").replace('"', "\\\"").chars()
                    .flat_map(|c| (space(c) || "+-!(){}[]^~*?:/|&".contains(c)).then_some('\\').into_iter().chain([c]))
                    .chain(['*']).collect(),
            },
            Self::And { left, right } => ("AND", left, right),
            Self::Or { left, right } => ("OR", left, right),
            Self::Not { left, right } => ("NOT", left, right),
        };
        format!("({} {operator} {})", left.compile(lucene), right.compile(lucene))
    }
}
fn and(value: &str) -> bool {
    value.eq_ignore_ascii_case("AND") || value.eq_ignore_ascii_case("ET") || matches!(value, "&" | "&&")
}
fn or(value: &str) -> bool {
    value.eq_ignore_ascii_case("OR") || value.eq_ignore_ascii_case("OU") || matches!(value, "|" | "||")
}
fn not(value: &str) -> bool {
    value.eq_ignore_ascii_case("NOT") || value.eq_ignore_ascii_case("NON")
}
fn tokens(query: &str) -> Vec<&str> {
    let (mut out, mut at) = (Vec::new(), 0);
    while let Some(c) = query[at..].chars().next().filter(|_| out.len() < 32) {
        let rest = &query[at..];
        let length = match c {
            _ if space(c) => {
                at += c.len_utf8();
                continue;
            }
            '(' | ')' => 1,
            '"' if rest[1..].contains('"') => rest[1..].find('"').unwrap() + 2,
            _ => rest.find(|c: char| space(c) || matches!(c, '(' | ')')).unwrap_or(rest.len()),
        };
        out.push(&rest[..length]);
        at += length;
    }
    // ("and not" is "not".)
    out.iter().enumerate().filter(|(i, t)| !and(t) || !out.get(i + 1).is_some_and(|t| not(t))).map(|(_, t)| *t).collect()
}
struct Parser<'a> {
    input: Vec<&'a str>,
    at: usize,
    valid: bool,
}
impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a str> {
        self.input.get(self.at).copied()
    }
    fn term(&mut self, raw: Option<&str>) -> SearchNode {
        let raw = raw.unwrap_or_else(|| {
            self.valid = false;
            NEVER
        });
        let quoted = raw.len() > 1 && raw.starts_with('"') && raw.ends_with('"');
        self.valid &= quoted || !raw.contains('"');
        let value = if quoted { &raw[1..raw.len() - 1] } else { raw };
        let prefix = !quoted && value.len() > 1 && value.ends_with('*');
        let value = if prefix { &value[..value.len() - 1] } else { value };
        SearchNode::Term { value: if value.is_empty() { NEVER } else { value }.to_owned(), prefix }
    }
    fn atom(&mut self) -> SearchNode {
        match self.peek() {
            None | Some(")") => self.term(None),
            Some("(") => {
                self.at += 1;
                let result = self.and();
                self.valid &= self.peek() == Some(")");
                self.at += usize::from(self.peek() == Some(")"));
                result
            }
            Some(value) => {
                self.at += 1;
                self.term(Some(value))
            }
        }
    }
    fn or(&mut self) -> SearchNode {
        let mut left = self.atom();
        while self.peek().is_some_and(or) {
            self.at += 1;
            left = SearchNode::Or { left: Box::new(left), right: Box::new(self.atom()) };
        }
        left
    }
    fn not(&mut self) -> SearchNode {
        let mut left = self.or();
        while let Some(raw) = self.peek().filter(|value| not(value) || value.len() > 1 && value.starts_with('-')) {
            self.at += 1;
            let right = match raw.strip_prefix('-') {
                Some(term) => self.term(Some(term)),
                None => self.or(),
            };
            left = SearchNode::Not { left: Box::new(left), right: Box::new(right) };
        }
        left
    }
    fn and(&mut self) -> SearchNode {
        let mut left = self.not();
        while self.peek().is_some_and(|t| t != ")") {
            self.at += usize::from(self.peek().is_some_and(and));
            left = SearchNode::And { left: Box::new(left), right: Box::new(self.not()) };
        }
        left
    }
}
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct SearchQuery {
    pub node: SearchNode,
    pub valid: bool,
    pub operators: bool,
    pub fts5: String,
    pub lucene: String,
    pub terms: Vec<String>,
}
pub fn compile(query: &str, columns: Option<&[String]>) -> SearchQuery {
    let mut parser = Parser { input: tokens(query), at: 0, valid: true };
    let node = if parser.input.is_empty() { parser.term(None) } else { parser.and() };
    parser.valid &= parser.at >= parser.input.len();
    let operators = query.chars().any(|c| "\"()&|".contains(c))
        || query.split(space).any(|value| and(value) || or(value) || not(value) || value.len() > 1 && value.starts_with('-'));
    let (mut fts5, lucene) = if trim(query).is_empty() { Default::default() } else { (node.compile(false), node.compile(true)) };
    if let Some(columns) = columns.filter(|columns| !columns.is_empty() && !fts5.is_empty()) {
        fts5 = format!("{{{}}} : ({fts5})", columns.join(" "));
    }
    let mut terms = Vec::new();
    node.terms(&mut terms);
    SearchQuery { node, valid: parser.valid, operators, fts5, lucene, terms }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn syntax_and_priority_are_shared() {
        for (query, expected) in [
            ("permit hunting", "(\"permit\" AND \"hunting\")"),
            (
                "permit AND hunting OR fishing",
                "(\"permit\" AND (\"hunting\" OR \"fishing\"))",
            ),
            ("permit ET NON fishing", "(\"permit\" NOT \"fishing\")"),
            ("custody -child", "(\"custody\" NOT \"child\")"),
        ] {
            assert_eq!(compile(query, None).fts5, expected);
        }
        assert_eq!(
            compile("permit and (hunting or fishing)", None).terms,
            vec!["permit", "hunting", "fishing"]
        );
        assert!(!compile("governing law", None).operators);
        assert!(compile("law OR equity", None).operators);
    }
    #[test]
    fn malformed_and_index_escaping() {
        for query in ["permit and (hunting", "permit \"city", "permit or", "  "] {
            assert!(!compile(query, None).valid, "{query}");
        }
        assert_eq!(compile("city*", None).fts5, "\"city\"*");
        assert_eq!(compile("city*", None).lucene, "city*");
        assert_eq!(
            compile("\"city municipality\"", None).fts5,
            "\"city municipality\""
        );
    }
}
