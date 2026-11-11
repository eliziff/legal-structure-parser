//! Beaver's shared CanLII-style query grammar and index dialects.
use legal_structure::{javascript_whitespace as space, trim_javascript_whitespace as trim};
use serde::{Deserialize, Serialize};
const NEVER: &str = "__beaver_no_search_match__";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SearchNode {
    Term { value: String, prefix: bool },
    And { left: Box<Self>, right: Box<Self> },
    Or { left: Box<Self>, right: Box<Self> },
    Not { left: Box<Self>, right: Box<Self> },
}
impl SearchNode {
    fn pair(kind: &str, left: Self, right: Self) -> Self {
        let (left, right) = (Box::new(left), Box::new(right));
        match kind {
            "and" => Self::And { left, right },
            "or" => Self::Or { left, right },
            _ => Self::Not { left, right },
        }
    }
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
        match self {
            Self::Term { value, prefix } => {
                let value = if lucene {
                    value.replace('\\', "\\\\").replace('"', "\\\"")
                } else {
                    value.replace('"', "\"\"")
                };
                if *prefix && lucene {
                    let mut escaped = String::new();
                    for c in value.chars() {
                        if space(c) || "+-!(){}[]^~*?:/|&".contains(c) {
                            escaped.push('\\');
                        }
                        escaped.push(c);
                    }
                    format!("{escaped}*")
                } else {
                    format!("\"{value}\"{}", if *prefix { "*" } else { "" })
                }
            }
            Self::And { left, right } => {
                format!("({} AND {})", left.compile(lucene), right.compile(lucene))
            }
            Self::Or { left, right } => {
                format!("({} OR {})", left.compile(lucene), right.compile(lucene))
            }
            Self::Not { left, right } => {
                format!("({} NOT {})", left.compile(lucene), right.compile(lucene))
            }
        }
    }
}
fn and(value: &str) -> bool {
    value.eq_ignore_ascii_case("AND")
        || value.eq_ignore_ascii_case("ET")
        || matches!(value, "&" | "&&")
}
fn or(value: &str) -> bool {
    value.eq_ignore_ascii_case("OR")
        || value.eq_ignore_ascii_case("OU")
        || matches!(value, "|" | "||")
}
fn not(value: &str) -> bool {
    value.eq_ignore_ascii_case("NOT") || value.eq_ignore_ascii_case("NON")
}
fn tokens(query: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < query.len() && out.len() < 32 {
        let c = query[at..].chars().next().unwrap();
        if space(c) {
            at += c.len_utf8();
            continue;
        }
        let start = at;
        if matches!(c, '(' | ')') {
            at += 1;
        } else if c == '"' && query[at + 1..].contains('"') {
            at += query[at + 1..].find('"').unwrap() + 2;
        } else {
            while at < query.len() {
                let c = query[at..].chars().next().unwrap();
                if space(c) || matches!(c, '(' | ')') {
                    break;
                }
                at += c.len_utf8();
            }
        }
        out.push(&query[start..at]);
    }
    out.iter()
        .enumerate()
        .filter(|(i, t)| !and(t) || !out.get(i + 1).is_some_and(|t| not(t)))
        .map(|(_, t)| *t)
        .collect()
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
        let quoted = raw.starts_with('"') && raw.ends_with('"') && raw.len() > 1;
        if raw.contains('"') && !quoted {
            self.valid = false;
        }
        let value = if quoted { &raw[1..raw.len() - 1] } else { raw };
        let prefix = !quoted && value.ends_with('*') && value.len() > 1;
        let value = if prefix {
            &value[..value.len() - 1]
        } else {
            value
        };
        SearchNode::Term {
            value: if value.is_empty() { NEVER } else { value }.to_owned(),
            prefix,
        }
    }
    fn atom(&mut self) -> SearchNode {
        let token = self.peek();
        self.at += 1;
        match token {
            None => self.term(None),
            Some(")") => {
                self.at -= 1;
                self.term(None)
            }
            Some("(") => {
                let result = self.and();
                if self.peek() == Some(")") {
                    self.at += 1;
                } else {
                    self.valid = false;
                }
                result
            }
            Some(value) => self.term(Some(value)),
        }
    }
    fn or(&mut self) -> SearchNode {
        let mut left = self.atom();
        while self.peek().is_some_and(or) {
            self.at += 1;
            left = SearchNode::pair("or", left, self.atom());
        }
        left
    }
    fn not(&mut self) -> SearchNode {
        let mut left = self.or();
        while self
            .peek()
            .is_some_and(|value| not(value) || (value.starts_with('-') && value.len() > 1))
        {
            let raw = self.peek().unwrap();
            self.at += 1;
            let right = if raw.starts_with('-') {
                self.term(Some(&raw[1..]))
            } else {
                self.or()
            };
            left = SearchNode::pair("not", left, right);
        }
        left
    }
    fn and(&mut self) -> SearchNode {
        let mut left = self.not();
        while self.peek().is_some_and(|t| t != ")") {
            if self.peek().is_some_and(and) {
                self.at += 1;
            }
            left = SearchNode::pair("and", left, self.not());
        }
        left
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct SearchQuery {
    pub node: SearchNode,
    pub valid: bool,
    pub operators: bool,
    pub fts5: String,
    pub lucene: String,
    pub terms: Vec<String>,
}
pub fn compile(query: &str, columns: Option<&[String]>) -> SearchQuery {
    let mut parser = Parser {
        input: tokens(query),
        at: 0,
        valid: true,
    };
    let node = if parser.input.is_empty() {
        parser.term(None)
    } else {
        parser.and()
    };
    if parser.at < parser.input.len() {
        parser.valid = false;
    }
    let operators = query.chars().any(|c| "\"()&|".contains(c))
        || query.split(space).any(|value| {
            and(value) || or(value) || not(value) || (value.starts_with('-') && value.len() > 1)
        });
    let empty = trim(query).is_empty();
    let mut fts5 = if empty {
        String::new()
    } else {
        node.compile(false)
    };
    if !empty {
        if let Some(columns) = columns.filter(|columns| !columns.is_empty()) {
            fts5 = format!("{{{}}} : ({fts5})", columns.join(" "));
        }
    }
    let lucene = if empty {
        String::new()
    } else {
        node.compile(true)
    };
    let mut terms = Vec::new();
    node.terms(&mut terms);
    SearchQuery {
        node,
        valid: parser.valid,
        operators,
        fts5,
        lucene,
        terms,
    }
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
