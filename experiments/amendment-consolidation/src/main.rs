mod delete_renumber;

use legal_structure::{
    analyze_instrument, normalize_document_locator, utf16_len, AuthoritativeTableCell, DocumentKind,
    DocumentStructure, NodeKind, ScalarText, StructureNode,
};
use legal_grammar_tables::{
    compile_ecmascript_pattern, compile_table_entry, expand_pattern, load_tables, CompiledGrammar,
};
use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    io::{self, Read},
    sync::OnceLock,
};
fn javascript_whitespace(character: char) -> bool {
    character == '\u{feff}' || (character != '\u{0085}' && character.is_whitespace())
}
fn js_trim(value: &str) -> &str { value.trim_matches(javascript_whitespace) }
fn equal_fold(left: &str, right: &str) -> bool {
    if left.is_ascii() && right.is_ascii() { left.eq_ignore_ascii_case(right) }
    else { left.to_lowercase() == right.to_lowercase() }
}
fn op(kind: &str, target: String, raw: &str) -> Value {
    json!({ "kind": kind, "target": target, "raw": raw })
}
fn put<T: Serialize>(op: &mut Value, key: &str, value: T) {
    op[key] = json!(value);
}
fn field<'a>(op: &'a Value, key: &str) -> Option<&'a str> {
    op.get(key).and_then(Value::as_str)
}
fn flag(op: &Value, key: &str) -> bool {
    op.get(key).and_then(Value::as_bool) == Some(true)
}
fn compact_label(value: &str) -> String {
    value
        .chars()
        .filter(|character| !javascript_whitespace(*character))
        .collect()
}
fn table_pattern(id: &str) -> String {
    let tables = load_tables().expect("valid legal grammar corpus");
    let entry = tables.get(id).expect("amendment provision grammar");
    expand_pattern(&entry.entry.pattern, &entry.defs).expect("expand amendment provision grammar")
}
fn compile(pattern: &str, flags: &str) -> Regex {
    compile_ecmascript_pattern("amendment", pattern, flags).expect("valid amendment grammar")
}
macro_rules! cached {
    ($slot:ident, $pattern:literal, $flags:literal) => {{
        static $slot: OnceLock<Regex> = OnceLock::new();
        $slot.get_or_init(|| compile($pattern, $flags))
    }};
}
struct Grammars {
    head: Regex,
    fr_head: Regex,
    fr_unclosed: CompiledGrammar,
    in_context: Regex,
    lead_ref: Regex,
    lead_anchored_ref: Regex,
    at_end_ref: Regex,
    after_before_ref: Regex,
    redesignate_ref: Regex,
    as_ref: Regex,
}
fn grammars() -> &'static Grammars {
    static VALUE: OnceLock<Grammars> = OnceLock::new();
    VALUE.get_or_init(|| {
        let en = table_pattern("provision.ref.en.anchored");
        let fr = table_pattern("provision.ref.fr.anchored");
        Grammars {
            head: compile(
                &format!(
                    r"(?:The\s+)?{en}\s+of\s+(?:the\s+)?.{{0,200}}?\s+is\s+(amended|repealed|replaced|redesignated|renumbered)|(?:The\s+)?{en}\s+is\s+(amended|repealed|replaced|redesignated|renumbered)"
                ),
                "i",
            ),
            fr_head: compile(
                &format!(
                    r"(?:les?\s+|la\s+|l['Ã¢â‚¬â„¢]\s?|aux?\s+|du\s+|de\s+la\s+|de\s+l['Ã¢â‚¬â„¢]\s?){fr}(?:\s+(?:de|du|des|de\s+la|de\s+l['Ã¢â‚¬â„¢]\s?)\s?.{{0,200}}?)?,?\s+(?:est|sont)\s+(remplacÃƒÂ©e?s?|abrogÃƒÂ©e?s?|modifiÃƒÂ©e?s?)"
                ),
                "i",
            ),
            fr_unclosed: compile_table_entry("provision.label.fr-unclosed")
                .expect("valid French provision label grammar"),
            in_context: compile(&format!(r"\bin\s+{en}\s*[,:]?"), "i"),
            lead_ref: compile(
                &format!(r"^(?:the\s+)?{en}"),
                "i",
            ),
            lead_anchored_ref: compile(
                &format!(r"^{en}"),
                "i",
            ),
            at_end_ref: compile(
                &format!(r"^\s*at\s+the\s+end\s+of\s+(?:the\s+)?{en}"),
                "i",
            ),
            after_before_ref: compile(
                &format!(r"(after|before)\s+{en}"),
                "i",
            ),
            redesignate_ref: compile(
                &format!(r"redesignat(?:ing|ed)\s+{en}.{{0,40}}?\bas\s+{en}"),
                "is",
            ),
            as_ref: compile(&format!(r"as\s+{en}"), "i"),
        }
    })
}
fn join_locator(head: &str, sub: Option<&str>) -> String {
    let head = compact_label(head);
    let sub = sub.map(compact_label).unwrap_or_default();
    let joined = if sub.starts_with('(') {
        format!("{head}{sub}")
    } else if sub.is_empty() {
        head
    } else {
        sub
    };
    if joined.is_empty() {
        String::new()
    } else {
        let normalized = normalize_document_locator(DocumentKind::Section, &joined);
        if normalized.is_empty() {
            format!("sec{}", joined.to_lowercase())
        } else {
            normalized
        }
    }
}
fn compact_label_fr(value: &str) -> String {
    grammars()
        .fr_unclosed
        .replace_all(&compact_label(value), "($1)")
        .into_owned()
}
#[derive(Clone)]
struct Quote { start: usize,
    end: usize,
    value: String, }
fn quotes(value: &str) -> Vec<Quote> {
    let mut found = Vec::new();
    let mut cursor = 0;
    while cursor < value.len() {
        let mut open = [("Ã¢â‚¬Å“", "Ã¢â‚¬Â"), ("``", "''"), ("Ã¢â‚¬Ëœ", "Ã¢â‚¬â„¢"), ("\"", "\"")]
            .into_iter()
            .filter_map(|(left, right)| {
                value[cursor..]
                    .find(left)
                    .map(|at| (cursor + at, left, right))
            })
            .collect::<Vec<_>>();
        open.sort_by_key(|item| item.0);
        let Some((start, left, right)) = open.into_iter().next() else {
            break;
        };
        let content = start + left.len();
        if let Some(close) = value[content..].find(right).map(|at| content + at) {
            found.push(Quote {
                start,
                end: close + right.len(),
                value: value[content..close].to_owned(),
            });
            cursor = close + right.len();
        } else {
            cursor = content;
        }
    }
    found
}
fn end_of_typographic_run(body: &str, coordinates: &ScalarText<'_>, open: usize) -> Option<usize> {
    let limit_utf16 = coordinates.utf16_at_byte(open).unwrap() + 60_000;
    let mut cursor = open + 'Ã¢â‚¬Å“'.len_utf8();
    let mut close = None;
    loop {
        let Some(next) = body[cursor..].find('Ã¢â‚¬Â').map(|at| cursor + at) else {
            break;
        };
        if coordinates.utf16_at_byte(next).unwrap() > limit_utf16 {
            break;
        }
        close = Some(next);
        let tail = &body[next + 'Ã¢â‚¬Â'.len_utf8()..];
        let mut count = 0;
        let mut reopened = false;
        for character in tail.chars() {
            if character == 'Ã¢â‚¬Å“' {
                reopened = true;
                break;
            }
            if count == 6 || !(javascript_whitespace(character) || ".;,".contains(character)) {
                break;
            }
            count += 1;
        }
        if !reopened {
            break;
        }
        cursor = next + 'Ã¢â‚¬Â'.len_utf8();
    }
    close
}
fn typographic_block(body: &str) -> Option<String> {
    let open = body.find('Ã¢â‚¬Å“')?;
    let close = end_of_typographic_run(body, &ScalarText::new(body), open)?;
    Some(body[open + 'Ã¢â‚¬Å“'.len_utf8()..close].to_owned())
}
fn quoted_block(body: &str) -> Option<String> {
    typographic_block(body)
        .filter(|value| !value.is_empty())
        .or_else(|| quotes(body).into_iter().next().map(|quote| quote.value))
}
fn mask_quoted_runs(body: &str) -> String {
    let mut bytes = body.as_bytes().to_vec();
    let coordinates = ScalarText::new(body);
    let mut cursor = 0;
    while let Some(open) = body[cursor..].find('Ã¢â‚¬Å“').map(|at| cursor + at) {
        let Some(close) = end_of_typographic_run(body, &coordinates, open) else {
            cursor = open + 3;
            continue;
        };
        for byte in &mut bytes[open + 3..close] {
            if *byte != b'\n' {
                *byte = b'x';
            }
        }
        cursor = close + 3;
    }
    String::from_utf8(bytes).expect("quote mask preserves UTF-8")
}
fn unquoted_block(body: &str) -> Option<String> {
    let colon = body.find(':')?;
    if colon > 160 {
        return None;
    }
    let mut kept = Vec::new();
    let mut saw_text = false;
    for line in body[colon + 1..].split('\n') {
        let trimmed = js_trim(line);
        if trimmed.is_empty() {
            kept.push("");
            continue;
        }
        if trimmed.to_lowercase().starts_with("marginal note:")
            || cached!(MARGINAL_FR, r"^note\s+marginale\s*:", "i").is_match(trimmed)
        {
            continue;
        }
        if saw_text && cached!(NEXT_INSTRUCTION, r"^\d{1,4}\s+\S", "").is_match(trimmed) {
            break;
        }
        if cached!(
            UNQUOTED_STOP,
            r"^(?:R\.S\.|S\.C\.|L\.R\.C\.|L\.C\.|L\.R\.)[,.\s]",
            ""
        )
        .is_match(trimmed)
        {
            break;
        }
        if saw_text
            && cached!(
                UNQUOTED_HEADING,
                r"^(?:[A-ZÃƒâ‚¬-ÃƒÅ¾][\wÃƒÂ -ÃƒÂ¿Ã¢â‚¬â„¢'-]*)(?:\s+(?:[a-zÃƒÂ -ÃƒÂ¿]{2,12}|[A-ZÃƒâ‚¬-ÃƒÅ¾][\wÃƒÂ -ÃƒÂ¿Ã¢â‚¬â„¢'-]*)){1,5}$",
                ""
            )
            .is_match(trimmed)
            && !trimmed.ends_with(['.', ';', ':', ',', ')'])
        {
            break;
        }
        kept.push(line);
        saw_text = true;
    }
    let block = js_trim(&kept.join("\n")).to_owned();
    (utf16_len(&block) >= 3).then_some(block)
}
#[derive(Clone)]
struct Clause { text: String,
    context: Option<String>, }
fn last_context(segment: &str) -> Option<String> {
    grammars()
        .in_context
        .captures_iter(segment)
        .last()
        .and_then(|capture| capture.get(1).map(|label| label.as_str().to_owned()))
}
fn split_clauses(body: &str) -> Vec<Clause> {
    let masked = mask_quoted_runs(body);
    let boundaries = cached!(BY, r"\bby\b\s+", "i")
        .find_iter(&masked)
        .collect::<Vec<_>>();
    let mut clauses = Vec::new();
    let mut pending = boundaries
        .first()
        .and_then(|first| last_context(&masked[..first.start()]));
    for (index, boundary) in boundaries.iter().enumerate() {
        let start = boundary.end();
        let end = boundaries
            .get(index + 1)
            .map_or(masked.len(), |next| next.start());
        let part = &body[start..end];
        if cached!(
            CLAUSE_VERB,
            r"^(?:strik|insert|add|redesignat|renumber|repeal|substitut|replac|delet)",
            "i"
        )
        .is_match(part.trim_start_matches(javascript_whitespace))
        {
            clauses.push(Clause {
                text: part.to_owned(),
                context: pending.clone(),
            });
        }
        if let Some(context) = last_context(&masked[start..end]) {
            pending = Some(context);
        }
    }
    if clauses.is_empty()
        && cached!(
            INSTRUCTION_VERB,
            r"striking|inserting|adding|repeal|replaced|renumber|redesignat|substitut",
            "i"
        )
        .is_match(body)
    {
        clauses.push(Clause {
            text: body.to_owned(),
            context: None,
        });
    }
    clauses
}
fn punctuation(value: &str) -> Option<&'static str> {
    let lower = value.to_lowercase();
    if lower == "comma" {
        Some(",")
    } else if lower == "semicolon" {
        Some(";")
    } else if lower == "period" {
        Some(".")
    } else {
        None
    }
}
fn op_from_clause(clause: &Clause, head: &str, raw: &str) -> Result<Value, &'static str> {
    let text = &clause.text;
    let target = join_locator(
        head,
        clause.context.as_deref().map(compact_label).as_deref(),
    );
    let every = cached!(
        EVERY,
        r"each\s+place\s+(?:it|such\s+term)\s+appears|wherever\s+appearing",
        "i"
    )
    .is_match(text);
    let lower = text.to_lowercase();
    let side = |word: &str| {
        if word.eq_ignore_ascii_case("before") {
            "before"
        } else {
            "after"
        }
    };
    if let Some(strike) = cached!(STRIKE, r"strik(?:ing|e)(?:\s+out)?\s+", "i").find(text) {
        let rest = &text[strike.end()..];
        if let Some(reference) = grammars().lead_ref.captures(rest) {
            if let Some(label) = reference.get(1) {
                let replace = cached!(INSERT_FOLLOWING, r"insert(?:ing)?\s+the\s+following", "i")
                    .is_match(rest);
                let mut op = op(
                    if replace {
                        "replace_provision"
                    } else {
                        "strike_provision"
                    },
                    join_locator(head, Some(&compact_label(label.as_str()))),
                    raw,
                );
                if let Some(value) = replace.then(|| quoted_block(rest)).flatten() {
                    put(&mut op, "newText", value);
                }
                return Ok(op);
            }
        }
        let found = quotes(rest);
        if let Some(struck) = found.first() {
            let after_quote = &rest[struck.end..];
            let end_of = grammars().at_end_ref.captures(after_quote);
            let scoped = end_of
                .as_ref()
                .and_then(|capture| capture.get(1))
                .map_or_else(
                    || target.clone(),
                    |label| join_locator(head, Some(&compact_label(label.as_str()))),
                );
            let insert = cached!(
                INSERT_SUBSTITUTE,
                r"\b(?:and\s+)?(?:insert(?:ing)?|substitut(?:ing|e))\b",
                "i"
            )
            .find(rest);
            if let Some(insert) = insert {
                if let Some(value) = quotes(&rest[insert.end()..]).first() {
                    let mut op = op("substitute_text", scoped, raw);
                    put(&mut op, "oldText", &struck.value);
                    put(&mut op, "newText", &value.value);
                    if every {
                        put(&mut op, "everyOccurrence", true);
                    }
                    if end_of.is_some() {
                        put(&mut op, "anchorLast", true);
                        put(&mut op, "wholeWord", true);
                    }
                    return Ok(op);
                }
            }
            let mut op = op("strike_text", scoped, raw);
            put(&mut op, "oldText", &struck.value);
            if every {
                put(&mut op, "everyOccurrence", true);
            }
            if end_of.is_some() {
                put(&mut op, "anchorLast", true);
                put(&mut op, "wholeWord", true);
            }
            return Ok(op);
        }
        if let Some(mark) = cached!(TERMINAL, r"^the\s+(period|comma|semicolon)\b", "i")
            .captures(rest)
            .and_then(|capture| punctuation(&capture[1]))
        {
            let insert = cached!(INSERT_AFTER, r"\b(?:and\s+)?insert(?:ing)?\b", "i")
                .find(rest)
                .and_then(|matched| quotes(&rest[matched.end()..]).first().cloned());
            let mut op = op(
                if insert.is_some() {
                    "substitute_text"
                } else {
                    "strike_text"
                },
                target,
                raw,
            );
            put(&mut op, "oldText", mark);
            if let Some(value) = insert {
                put(&mut op, "newText", value.value);
            }
            put(&mut op, "anchorLast", true);
            return Ok(op);
        }
        return Err("strike clause without quoted text or provision ref");
    }
    if let Some(insert) = cached!(INSERT, r"insert(?:ing)?\s+", "i").find(text) {
        let rest = &text[insert.end()..];
        let found = quotes(rest);
        if let (Some(value), Some(placement)) = (
            found.first(),
            cached!(PLACEMENT, r"\b(after|before)\b", "i").captures(rest),
        ) {
            let position = side(&placement[1]);
            let tail = &rest[placement.get(0).unwrap().end()..];
            if let Some(anchor) = quotes(tail).first() {
                let mut op = op("insert_text", target, raw);
                put(&mut op, "newText", &value.value);
                put(&mut op, "position", position);
                put(&mut op, "anchorText", &anchor.value);
                if every {
                    put(&mut op, "everyOccurrence", true);
                }
                return Ok(op);
            }
            if let Some(mark) = cached!(
                TERMINAL_ANCHOR,
                r"^\s*the\s+(period|comma|semicolon)\b",
                "i"
            )
            .captures(tail)
            .and_then(|capture| punctuation(&capture[1]))
            {
                let mut op = op("insert_text", target, raw);
                put(&mut op, "newText", &value.value);
                put(&mut op, "position", position);
                put(&mut op, "anchorText", mark);
                put(&mut op, "anchorLast", true);
                return Ok(op);
            }
        }
        if let Some(reference) = grammars().after_before_ref.captures(rest) {
            if cached!(THE_FOLLOWING, r"the\s+following", "i").is_match(rest) {
                let mut op = op("add_provision", target, raw);
                put(&mut op, "position", side(&reference[1]));
                put(
                    &mut op,
                    "afterChild",
                    join_locator(head, Some(&compact_label(&reference[2]))),
                );
                if let Some(value) = found.first() {
                    put(&mut op, "newText", &value.value);
                }
                return Ok(op);
            }
        }
        return Err(if found.is_empty() {
            "insert clause without quoted text"
        } else {
            "insert clause without placement anchor"
        });
    }
    if let Some(adding) = cached!(ADDING, r"adding\s+", "i").find(text) {
        let rest = &text[adding.end()..];
        if let Some(value) = quotes(rest).first().filter(|value| value.start == 0) {
            if let Some(reference) = grammars().at_end_ref.captures(&rest[value.end..]) {
                let mut op = op(
                    "append_text",
                    join_locator(head, Some(&compact_label(&reference[1]))),
                    raw,
                );
                put(&mut op, "newText", &value.value);
                return Ok(op);
            }
        }
    }
    if let Some(adding) = cached!(
        ADD_FOLLOWING,
        r"adding\s+the\s+following\s+(after|before)\s+",
        "i"
    )
    .captures(text)
    {
        let tail = &text[adding.get(0).unwrap().end()..];
        if let Some(reference) = grammars().lead_anchored_ref.captures(tail) {
            let mut op = op("add_provision", target, raw);
            put(&mut op, "position", side(&adding[1]));
            put(
                &mut op,
                "afterChild",
                join_locator(head, Some(&compact_label(&reference[1]))),
            );
            if let Some(value) = quoted_block(text).or_else(|| unquoted_block(text)) {
                put(&mut op, "newText", value);
            }
            return Ok(op);
        }
        return Err("adding-the-following without provision ref");
    }
    if cached!(ADD_AT_END, r"adding\s+at\s+the\s+end", "i").is_match(text) {
        let mut op = op("add_at_end", target, raw);
        if let Some(value) = quoted_block(text).or_else(|| unquoted_block(text)) {
            put(&mut op, "newText", value);
        }
        return Ok(op);
    }
    if lower.contains("redesignat") {
        if let Some(reference) = grammars().redesignate_ref.captures(text) {
            let mut op = op(
                "redesignate",
                join_locator(head, Some(&compact_label(&reference[1]))),
                raw,
            );
            put(&mut op, "newLabel", compact_label(&reference[2]));
            return Ok(op);
        }
    }
    Err("unrecognized amendment clause")
}
struct Head { label: String,
    verb: String,
    start: usize,
    end: usize,
    french: bool, }
fn capture_head(capture: Captures<'_>, french: bool) -> Option<Head> {
    let whole = capture.get(0)?;
    let (label, verb) = if french {
        (capture.get(1)?, capture.get(2)?)
    } else if let (Some(label), Some(verb)) = (capture.get(1), capture.get(2)) {
        (label, verb)
    } else {
        (capture.get(3)?, capture.get(4)?)
    };
    let verb = if french {
        let lower = verb.as_str().to_lowercase();
        if lower.starts_with("remplac") {
            "replaced"
        } else if lower.starts_with("abrog") {
            "repealed"
        } else {
            "amended"
        }
    } else {
        verb.as_str()
    };
    Some(Head {
        label: if french {
            compact_label_fr(label.as_str())
        } else {
            compact_label(label.as_str())
        },
        verb: verb.to_lowercase(),
        start: whole.start(),
        end: whole.end(),
        french,
    })
}
fn parse_amendment_instructions(text: &str) -> Value {
    let mut heads = grammars()
        .head
        .captures_iter(text)
        .filter_map(|capture| capture_head(capture, false))
        .chain(
            grammars()
                .fr_head
                .captures_iter(text)
                .filter_map(|capture| capture_head(capture, true)),
        )
        .collect::<Vec<_>>();
    heads.sort_by_key(|head| head.start);
    let (mut ops, mut unparsed) = (Vec::new(), Vec::new());
    let miss = |excerpt: &str, reason: &str| json!({ "excerpt": excerpt, "reason": reason });
    for (index, head) in heads.iter().enumerate() {
        let body_end = heads.get(index + 1).map_or(text.len(), |next| next.start);
        let body = &text[head.end..body_end];
        let raw_end = text[head.end..body_end]
            .char_indices()
            .scan(0, |units, (byte, character)| {
                *units += character.len_utf16();
                (*units <= 240).then_some(head.end + byte + character.len_utf8())
            })
            .last()
            .unwrap_or(head.end);
        let raw = js_trim(&text[head.start..raw_end]);
        let prefix_start = text[..head.start]
            .char_indices()
            .rev()
            .scan(0, |units, (byte, character)| {
                *units += character.len_utf16();
                (*units <= if head.french { 90 } else { 30 }).then_some(byte)
            })
            .last()
            .unwrap_or(head.start);
        let scoped = if head.french {
            cached!(
                FR_SCOPED,
                r"(?:\bpassage\s+d[eu]\b|\bdÃƒÂ©finitions?\s+d(?:e|u|es)\b).{0,80}$",
                "is"
            )
            .is_match(&text[prefix_start..head.start])
        } else {
            cached!(
                SCOPED,
                r"\b(?:portion|heading|marginal\s+note|description|title)\s+of\s*$|\bdefinitions?\s+[\w'Ã¢â‚¬â„¢\s-]{0,60}in\s*$",
                "i"
            )
            .is_match(&text[prefix_start..head.start])
        };
        if scoped {
            unparsed.push(miss(
                raw,
                "scoped amendment (portion/heading) Ã¢â‚¬â€ not applied",
            ));
            continue;
        }
        let target = join_locator(&head.label, None);
        if head.verb == "repealed" {
            ops.push(op("repeal_provision", target, raw));
            continue;
        }
        if head.verb == "replaced" {
            if let Some(block) = quoted_block(body).or_else(|| unquoted_block(body)) {
                let mut op = op("replace_provision", target, raw);
                put(&mut op, "newText", block);
                ops.push(op);
            } else {
                unparsed.push(miss(raw, "replaced-by without following block"));
            }
            continue;
        }
        if head.verb == "redesignated" || head.verb == "renumbered" {
            if let Some(reference) = grammars().as_ref.captures(body) {
                let mut op = op("redesignate", target, raw);
                put(&mut op, "newLabel", compact_label(&reference[1]));
                ops.push(op);
            } else {
                unparsed.push(miss(raw, "redesignation without new label"));
            }
            continue;
        }
        if cached!(READ_FOLLOWS, r"to\s+read\s+as\s+follows", "i")
            .is_match(&body[..body.char_indices().nth(80).map_or(body.len(), |(at, _)| at)])
        {
            if let Some(block) = quoted_block(body).or_else(|| unquoted_block(body)) {
                let mut op = op("replace_provision", target, raw);
                put(&mut op, "newText", block);
                ops.push(op);
                continue;
            }
        }
        let clauses = split_clauses(body);
        if clauses.is_empty() {
            unparsed.push(miss(raw, "amended-by without clauses"));
            continue;
        }
        for clause in clauses {
            match op_from_clause(&clause, &head.label, raw) {
                Ok(op) => ops.push(op),
                Err(reason) => unparsed.push(miss(
                    &js_trim(&clause.text).chars().take(160).collect::<String>(),
                    reason,
                )),
            }
        }
    }
    json!({ "ops": ops, "unparsed": unparsed })
}
struct Analyzed { structure: DocumentStructure, }
fn analyze(text: &str, reconstruct_lineation: bool) -> Result<Analyzed, legal_structure::EngineError> {
    let structure = analyze_instrument(
        text,
        String::new(),
        &[] as &[AuthoritativeTableCell],
        reconstruct_lineation,
    )?;
    Ok(Analyzed { structure })
}
fn literal_pattern(literal: &str) -> Regex {
    let mut pattern = String::new();
    let mut plain = String::new();
    let mut whitespace = false;
    for character in js_trim(literal).chars() {
        if javascript_whitespace(character) {
            if !plain.is_empty() {
                pattern.push_str(&regex::escape(&plain));
                plain.clear();
            }
            whitespace = true;
        } else {
            if whitespace {
                pattern.push_str(r"\s+");
                whitespace = false;
            }
            plain.push(character);
        }
    }
    pattern.push_str(&regex::escape(&plain));
    compile(&pattern, "")
}
fn find_in_span(
    text: &str,
    coordinates: &ScalarText<'_>,
    span: (usize, usize),
    literal: &str,
) -> Vec<(usize, usize)> {
    let Some(low) = coordinates.byte_at_utf16(span.0) else {
        return Vec::new();
    };
    let Some(high) = coordinates.byte_at_utf16(span.1) else {
        return Vec::new();
    };
    literal_pattern(literal)
        .find_iter(&text[low..high])
        .map(|found| {
            (
                coordinates.utf16_at_byte(low + found.start()).unwrap(),
                coordinates.utf16_at_byte(low + found.end()).unwrap(),
            )
        })
        .collect()
}
struct Target { span: (usize, usize),
    node: bool, }
fn resolve_target(structure: &DocumentStructure, target: &str, length: usize) -> Option<Target> {
    if target.is_empty() {
        return Some(Target {
            span: (0, length),
            node: false,
        });
    }
    let normalized = normalize_document_locator(DocumentKind::Section, target);
    let (mut raw_match, mut normalized_match) = (None, None);
    let (mut raw_ambiguous, mut normalized_ambiguous) = (false, false);
    let mut prose = 0;
    for node in &structure.nodes {
        if !matches!(
            node.kind,
            NodeKind::Paragraph
                | NodeKind::Prose
                | NodeKind::Page
                | NodeKind::Section
                | NodeKind::Footnote
                | NodeKind::Table
                | NodeKind::Row
                | NodeKind::Cell
        ) {
            continue;
        }
        let primary = if node.kind == NodeKind::Prose {
            prose += 1;
            Cow::Owned(format!("par{prose}"))
        } else {
            let Some(label) = &node.label else { continue };
            Cow::Borrowed(label.as_str())
        };
        let (mut matches_raw, mut matches_normalized) = (
            equal_fold(&primary, target),
            !normalized.is_empty() && equal_fold(&primary, &normalized),
        );
        for label in node.aliases.iter().flatten().chain(node.anchor.iter()) {
            matches_raw |= equal_fold(label, target);
            matches_normalized |= !normalized.is_empty() && equal_fold(label, &normalized);
        }
        if matches_raw {
            raw_ambiguous |= raw_match.replace(node).is_some();
        }
        if matches_normalized {
            normalized_ambiguous |= normalized_match.replace(node).is_some();
        }
    }
    let found = (!raw_ambiguous).then_some(raw_match).flatten().or_else(|| {
        (!normalized_ambiguous)
            .then_some(normalized_match)
            .flatten()
    });
    if let Some(node) = found {
        return Some(Target {
            span: (node.range.start, node.range.end),
            node: true,
        });
    }
    None
}
fn has_text(value: Option<&str>) -> bool {
    value.is_some_and(|value| !js_trim(value).is_empty())
}
fn is_word_unit(units: &[u16], at: usize) -> bool {
    static WORD: OnceLock<Regex> = OnceLock::new();
    let Some(unit) = units.get(at).copied() else {
        return false;
    };
    if (0xd800..=0xdfff).contains(&unit) {
        return false;
    }
    char::from_u32(unit.into()).is_some_and(|character| {
        WORD.get_or_init(|| Regex::new(r"^[\p{L}\p{N}]$").unwrap())
            .is_match(&character.to_string())
    })
}
fn ensure_block(value: &str) -> String {
    format!("{}\n", value.trim_end_matches(javascript_whitespace))
}
struct Splice { start: usize,
    end: usize,
    replacement: String,
    receipt: Value, }
fn amend_failure(op: &Value, code: &str, detail: String) -> Value {
    json!({ "op": op, "code": code, "detail": detail })
}
fn apply_amend_ops(
    source: &str,
    ops: Vec<Value>,
    reconstruct_lineation: bool,
) -> Result<Value, legal_structure::EngineError> {
    let before = analyze(source, reconstruct_lineation)?;
    let coordinates = ScalarText::new(source);
    let length = coordinates.utf16_len();
    let units = source.encode_utf16().collect::<Vec<_>>();
    let mut splices = Vec::<Splice>::new();
    let mut failures = Vec::new();
    let mut push = |op: &Value, start: usize, end: usize, replacement: String| {
        splices.push(Splice {
            start,
            end,
            receipt: json!({
                "op": op, "start": start, "end": end,
                "removed": coordinates.slice_utf16(start..end).unwrap_or_default(), "inserted": replacement
            }),
            replacement,
        });
    };
    for op in &ops {
        macro_rules! reject {
            ($code:literal, $detail:expr $(,)?) => {{
                failures.push(amend_failure(op, $code, $detail));
                continue;
            }};
        }
        let target_name = field(op, "target").unwrap_or("");
        let Some(target) = resolve_target(&before.structure, target_name, length) else {
            reject!("target_not_found", target_name.to_owned());
        };
        let fail = |code, detail: String| amend_failure(op, code, detail);
        match field(op, "kind").unwrap_or("") {
            "strike_text" | "substitute_text" => {
                let Some(old) = field(op, "oldText").filter(|old| !js_trim(old).is_empty()) else {
                    reject!("old_text_not_found", "empty quoted text".to_owned());
                };
                let mut hits = find_in_span(source, &coordinates, target.span, old);
                if flag(op, "wholeWord") {
                    hits.retain(|(start, end)| {
                        (*start == 0 || !is_word_unit(&units, *start - 1))
                            && (*end >= units.len() || !is_word_unit(&units, *end))
                    });
                }
                if hits.is_empty() {
                    reject!("old_text_not_found", old.chars().take(80).collect());
                }
                if hits.len() > 1 && !flag(op, "everyOccurrence") && !flag(op, "anchorLast") {
                    reject!(
                        "old_text_ambiguous",
                        format!(
                            "{} occurrences of \"{}\" in {}",
                            hits.len(),
                            old.chars().take(60).collect::<String>(),
                            if target_name.is_empty() {
                                "document"
                            } else {
                                target_name
                            }
                        ),
                    );
                }
                let chosen: &[(_, _)] = if flag(op, "everyOccurrence") {
                    &hits
                } else if flag(op, "anchorLast") {
                    &hits[hits.len() - 1..]
                } else {
                    &hits[..1]
                };
                let replacement = if field(op, "kind") == Some("substitute_text") {
                    field(op, "newText").unwrap_or("").to_owned()
                } else {
                    String::new()
                };
                for &(start, end) in chosen {
                    push(op, start, end, replacement.clone());
                }
            }
            "insert_text" => {
                let (Some(anchor), Some(value)) = (field(op, "anchorText"), field(op, "newText"))
                else {
                    reject!("anchor_not_found", "missing anchor or text".to_owned());
                };
                if js_trim(anchor).is_empty() {
                    reject!("anchor_not_found", "missing anchor or text".to_owned());
                }
                let hits = find_in_span(source, &coordinates, target.span, anchor);
                if hits.is_empty() {
                    reject!("anchor_not_found", anchor.chars().take(80).collect());
                }
                if hits.len() > 1 && !flag(op, "everyOccurrence") && !flag(op, "anchorLast") {
                    reject!(
                        "anchor_ambiguous",
                        format!(
                            "{} occurrences of \"{}\"",
                            hits.len(),
                            anchor.chars().take(60).collect::<String>()
                        ),
                    );
                }
                let chosen = if flag(op, "everyOccurrence") {
                    &hits[..]
                } else if flag(op, "anchorLast") {
                    &hits[hits.len() - 1..]
                } else {
                    &hits[..1]
                };
                let glue = if matches!(anchor, "." | "," | ";")
                    || value.starts_with(' ')
                    || value.starts_with('\n')
                {
                    ""
                } else {
                    " "
                };
                for &(start, end) in chosen {
                    let before = field(op, "position") == Some("before");
                    push(
                        op,
                        if before { start } else { end },
                        if before { start } else { end },
                        if before {
                            format!("{value}{glue}")
                        } else {
                            format!("{glue}{value}")
                        },
                    );
                }
            }
            "replace_provision" => match field(op, "newText") {
                Some(value) => push(op, target.span.0, target.span.1, ensure_block(value)),
                None => failures.push(fail("missing_new_text", target_name.to_owned())),
            },
            "strike_provision" | "repeal_provision" => {
                if !target.node {
                    failures.push(fail(
                        "target_not_found",
                        "cannot repeal whole document".to_owned(),
                    ));
                } else {
                    push(op, target.span.0, target.span.1, String::new());
                }
            }
            "add_at_end" => match field(op, "newText") {
                Some(value) => push(
                    op,
                    target.span.1,
                    target.span.1,
                    format!("\n{}", ensure_block(value)),
                ),
                None => failures.push(fail("missing_new_text", target_name.to_owned())),
            },
            "append_text" => {
                let Some(value) = field(op, "newText").filter(|value| !js_trim(value).is_empty())
                else {
                    reject!("missing_new_text", target_name.to_owned());
                };
                let trimmed = coordinates
                    .slice_utf16(target.span.0..target.span.1)
                    .unwrap_or_default()
                    .trim_end_matches(javascript_whitespace);
                let terminal = trimmed.chars().next_back();
                let at = target.span.0 + utf16_len(trimmed);
                match terminal {
                    Some('.') => push(op, at - 1, at, format!("; {value}")),
                    Some(';') => push(op, at, at, format!(" {value}")),
                    _ => failures.push(fail(
                        "unsupported_apply",
                        format!(
                            "append_text needs a \".\" or \";\" terminal, saw {}",
                            serde_json::to_string(
                                &terminal.map(|value| value.to_string()).unwrap_or_default()
                            )
                            .unwrap()
                        ),
                    )),
                }
            }
            "add_provision" => {
                let Some(value) = field(op, "newText") else {
                    reject!("missing_new_text", target_name.to_owned());
                };
                let child_name = field(op, "afterChild");
                let child = child_name.and_then(|label| {
                    resolve_target(&before.structure, label, length).filter(|target| target.node)
                });
                if child_name.is_some() && child.is_none() {
                    reject!("target_not_found", child_name.unwrap().to_owned());
                }
                let at = child.map_or(target.span.1, |child| {
                    if field(op, "position") == Some("before") {
                        child.span.0
                    } else {
                        child.span.1
                    }
                });
                push(op, at, at, format!("\n{}", ensure_block(value)));
            }
            "redesignate" => {
                let Some(label) = field(op, "newLabel").filter(|_| target.node) else {
                    reject!(
                        "unsupported_apply",
                        "redesignation needs a labelled node".to_owned(),
                    );
                };
                let lead_end = (target.span.0 + 40).min(target.span.1);
                let lead = coordinates
                    .slice_utf16(target.span.0..lead_end)
                    .unwrap_or_default();
                let Some(token) = cached!(
                    LEAD_TOKEN,
                    r"^(\s*)(\([^\s()]{1,12}\)|\d+[A-Za-z]?(?:\.\d+)*\.?)",
                    ""
                )
                .captures(lead) else {
                    reject!(
                        "unsupported_apply",
                        "no leading label token found".to_owned(),
                    );
                };
                let start = target.span.0 + utf16_len(&token[1]);
                push(op, start, start + utf16_len(&token[2]), label.to_owned());
            }
            _ => failures.push(fail(
                "unsupported_apply",
                "unsupported amendment operation".to_owned(),
            )),
        }
    }
    splices.sort_by_key(|splice| (splice.start, splice.end));
    let mut accepted = Vec::<Splice>::new();
    for splice in splices {
        if let Some(previous) = accepted
            .last()
            .filter(|previous| splice.start < previous.end)
        {
            failures.push(json!({
                "op": splice.receipt["op"].clone(), "code": "overlapping_ops",
                "detail": format!("overlaps op at {}-{}", previous.start, previous.end)
            }));
        } else {
            accepted.push(splice);
        }
    }
    let mut text = source.to_owned();
    for splice in accepted.iter().rev() {
        let start = coordinates
            .byte_at_utf16(splice.start)
            .expect("splice start boundary");
        let end = coordinates
            .byte_at_utf16(splice.end)
            .expect("splice end boundary");
        text.replace_range(start..end, &splice.replacement);
    }
    let after_owned;
    let after = if text == source {
        &before
    } else {
        after_owned = analyze(&text, reconstruct_lineation)?;
        &after_owned
    };
    let ladder = |analysis: &Analyzed| {
        analysis
            .structure
            .diagnostics
            .iter()
            .filter(|item| item.code == "instrument_ladder_violation")
            .count()
    };
    let (mut present, mut missing, mut gone, mut lingers) = (0, 0, 0, 0);
    let after_coordinates = ScalarText::new(&text);
    let after_length = after_coordinates.utf16_len();
    for splice in &accepted {
        let op = &splice.receipt["op"];
        if has_text(field(op, "newText")) {
            if find_in_span(
                &text,
                &after_coordinates,
                (0, after_length),
                field(op, "newText").unwrap(),
            )
            .is_empty()
            {
                missing += 1;
            } else {
                present += 1;
            }
        }
        if matches!(field(op, "kind"), Some("strike_text" | "substitute_text"))
            && has_text(field(op, "oldText"))
        {
            let lingering = resolve_target(
                &after.structure,
                field(op, "target").unwrap_or(""),
                after_length,
            )
            .map(|target| {
                find_in_span(
                    &text,
                    &after_coordinates,
                    target.span,
                    field(op, "oldText").unwrap(),
                )
                .len()
            })
            .unwrap_or(0);
            if lingering == 0 {
                gone += 1;
            } else {
                lingers += 1;
            }
        }
    }
    Ok(json!({
        "text": text, "applied": accepted.into_iter().map(|splice| splice.receipt).collect::<Vec<_>>(),
        "failures": failures, "verification": {
            "newTextPresent": present, "newTextMissing": missing, "oldTextGone": gone, "oldTextLingers": lingers,
            "ladderViolationsBefore": ladder(&before), "ladderViolationsAfter": ladder(after)
        }
    }))
}
fn consolidate_amendment(
    source: &str,
    amendment: &str,
    reconstruct_lineation: bool,
) -> Result<Value, legal_structure::EngineError> {
    let parse = parse_amendment_instructions(amendment);
    let ops = parse["ops"].as_array().cloned().unwrap_or_default();
    let mut result = apply_amend_ops(source, ops, reconstruct_lineation)?;
    result["parse"] = parse;
    Ok(result)
}
fn default_true() -> bool { true }
#[derive(Deserialize)]
#[serde(tag = "operation")]
enum Input {
    #[serde(rename = "consolidate")]
    Consolidate {
        source: String,
        amendment: String,
        #[serde(default, rename = "reconstructLineation")]
        reconstruct_lineation: bool,
    },
    #[serde(rename = "delete_and_renumber")]
    DeleteAndRenumber {
        source: String,
        target: String,
        #[serde(default = "default_true", rename = "reconstructLineation")]
        reconstruct_lineation: bool,
    },
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let input: Input = serde_json::from_str(&input)?;
    let result = match input {
        Input::Consolidate {
            source,
            amendment,
            reconstruct_lineation,
        } => consolidate_amendment(&source, &amendment, reconstruct_lineation)?,
        Input::DeleteAndRenumber {
            source,
            target,
            reconstruct_lineation,
        } => delete_renumber::delete_provision_and_renumber_siblings(
            &source,
            &target,
            reconstruct_lineation,
        )?,
    };
    serde_json::to_writer(io::stdout().lock(), &result)?;
    Ok(())
}
