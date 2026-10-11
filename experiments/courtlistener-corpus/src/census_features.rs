//! Discovery counts, deliberately separate from the structure engine and legal-role gold.
use scraper::{Html, Node};
use std::collections::{BTreeMap, HashMap, HashSet};

pub(super) const SCOPE: &str = "Markup observations use scraper's HTML5 fragment DOM: implied elements, foster parenting, case folding, entity decoding and duplicate-attribute repair can differ from original XML/HTML. Parser error details are not collected. Paths follow nearest element parents through template fragments. Qualified DOM names and declared xmlns values are inventoried without validating namespace semantics. Class/attribute/CSS values are discovery forms, not computed styles, visibility or legal roles; CSS declaration splitting is approximate. Keyword scopes are explicitly lexical cues, not adjudicated opinion/note ownership. Local links compare trimmed, entity-decoded literal #fragments to DOM IDs and A names; percent decoding, base URLs, external context, whole served-page uniqueness and navigation are unmeasured. Plain text is measured directly, never parsed as markup. Variable identifiers and long values are normalized. Long pure alphanumeric IDs with at least three letter/digit transitions use opaque prefix/length forms; party and numbered-party attribute values use shapes/lengths instead of name literals. Exact source values still drive local link equality and lexical cues. Bounded complete source witnesses, not feature tokens, preserve exact original bytes/values. Counts are occurrences within one supplied rendition, not distinct legal documents or semantic completeness.";

type Counts = HashMap<String, u64>;
const CUES: [&str; 12] = [
    "opinion",
    "author",
    "note",
    "reference",
    "page",
    "paragraph",
    "heading",
    "section",
    "citation",
    "headmatter",
    "attachment",
    "list",
];

fn add(counts: &mut Counts, token: impl Into<String>) {
    *counts.entry(token.into()).or_default() += 1;
}

fn bucket(n: usize) -> &'static str {
    match n {
        0 => "0",
        1 => "1",
        2..=3 => "2-3",
        4..=7 => "4-7",
        8..=15 => "8-15",
        16..=31 => "16-31",
        32..=63 => "32-63",
        64..=127 => "64-127",
        _ => "128+",
    }
}

// Collapse variable digit/long hexadecimal runs; keep unfamiliar surrounding syntax.
fn form(value: &str) -> String {
    let mut out = String::new();
    let mut at = 0;
    let bytes = value.as_bytes();
    while at < value.len() && out.len() < 96 {
        let ch = value[at..].chars().next().unwrap();
        if ch.is_ascii_hexdigit() {
            let mut end = at;
            while bytes.get(end).is_some_and(u8::is_ascii_hexdigit) {
                end += 1;
            }
            if end - at >= 12 {
                out.push_str("{hex}");
                at = end;
                continue;
            }
        }
        if ch.is_ascii_digit() {
            out.push('#');
            at += 1;
            while bytes.get(at).is_some_and(u8::is_ascii_digit) {
                at += 1;
            }
        } else {
            if ch.is_whitespace() {
                if !out.ends_with(' ') {
                    out.push(' ');
                }
            } else {
                out.push(ch.to_ascii_lowercase());
            }
            at += ch.len_utf8();
        }
    }
    if at < value.len() {
        out.push('…');
    }
    out
}

fn identifier(value: &str) -> String {
    if value.len() == 36
        && value.bytes().enumerate().all(|(i, ch)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                ch == b'-'
            } else {
                ch.is_ascii_hexdigit()
            }
        })
    {
        return "uuid-form".to_owned();
    }
    // Generated base36-style IDs alternate letters and digits; replacing only digits
    // preserves their random letter payload and creates one census key per instance.
    if value.len() >= 16 && value.bytes().all(|ch| ch.is_ascii_alphanumeric()) {
        let transitions = value
            .as_bytes()
            .windows(2)
            .filter(|pair| pair[0].is_ascii_digit() != pair[1].is_ascii_digit())
            .count();
        if transitions >= 3 {
            let prefix_len = value.bytes().take_while(u8::is_ascii_alphabetic).count();
            let prefix = if prefix_len <= 2 {
                value[..prefix_len].to_ascii_lowercase()
            } else {
                format!("letters-{}", bucket(prefix_len))
            };
            return format!("opaque-alphanumeric:{prefix}:{}", bucket(value.len()));
        }
    }
    if !value.is_empty() && value.bytes().all(|ch| ch.is_ascii_alphabetic()) {
        // CAP's opaque short alphabetical anchors must not create one feature per ID.
        return format!("alphabetic:{}", bucket(value.len()));
    }
    form(value)
}

fn instance_text_attribute(name: &str) -> bool {
    name.strip_prefix("party")
        .is_some_and(|suffix| suffix.bytes().all(|ch| ch.is_ascii_digit()))
}

fn value_shape(value: &str) -> &'static str {
    if value.is_empty() {
        "empty"
    } else if value.chars().all(char::is_whitespace) {
        "whitespace"
    } else if value.parse::<i64>().is_ok() {
        "integer"
    } else if value.parse::<f64>().is_ok() {
        "number"
    } else if value.bytes().all(|ch| ch.is_ascii_alphabetic()) {
        "alphabetic"
    } else if value
        .bytes()
        .all(|ch| ch.is_ascii_alphanumeric() || b"_:-.".contains(&ch))
    {
        "identifier"
    } else if value.contains("://") {
        "url"
    } else if value.chars().any(char::is_whitespace) {
        "spaced"
    } else {
        "other"
    }
}

fn cues(value: &str) -> u16 {
    let s = value.to_ascii_lowercase();
    let predicates = [
        s.contains("opinion") || s.contains("dissent_text") || s.contains("concurrence_text"),
        s.contains("author") || s.contains("byline") || s == "judges" || s == "attorneys",
        s.contains("note") || s.starts_with("ftn") || s.starts_with("fn"),
        s.contains("reference")
            || s.contains("jumpback")
            || s.contains("notemark")
            || s.ends_with("_ref"),
        s.contains("page") || s.contains("pagination"),
        s.contains("paragraph")
            || s.starts_with("par")
            || (s.starts_with('p') && s.as_bytes().get(1).is_some_and(u8::is_ascii_digit)),
        s.contains("heading")
            || (s.len() == 2 && s.starts_with('h') && s.as_bytes()[1].is_ascii_digit()),
        s.contains("section") || s == "article" || s == "casebody",
        s.contains("citation") || s.contains("cite") || s == "ref",
        s.contains("headmatter")
            || s.contains("head-matter")
            || s.contains("prelim")
            || s.contains("caption"),
        s.contains("appendix") || s.contains("addendum") || s.contains("attachment"),
        matches!(s.as_str(), "ul" | "ol" | "li" | "dl" | "dt" | "dd") || s.contains("list"),
    ];
    predicates
        .iter()
        .enumerate()
        .fold(0, |mask, (i, yes)| mask | ((*yes as u16) << i))
}

#[derive(Clone, Copy)]
struct Context<'a> {
    tag: &'a str,
    parent_tag: &'a str,
    opinion: Option<usize>,
    ancestor_cues: u16,
    own_cues: u16,
}

#[derive(Clone, Copy)]
struct Target<'a> {
    count: usize,
    tag: &'a str,
    cues: u16,
}

fn target<'a>(map: &mut HashMap<&'a str, Target<'a>>, name: &'a str, tag: &'a str, cues: u16) {
    let entry = map.entry(name).or_insert(Target {
        count: 0,
        tag,
        cues,
    });
    entry.count += 1;
}

fn css(counts: &mut Counts, style: &str) {
    for declaration in style.split(';').filter(|part| !part.trim().is_empty()) {
        let Some((property, value)) = declaration.split_once(':') else {
            add(counts, "dom:css:unparsed-declaration");
            continue;
        };
        let property = property.trim().to_ascii_lowercase();
        if property.is_empty()
            || !property
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || b"_-".contains(&ch))
        {
            add(counts, "dom:css:unparsed-property");
            continue;
        }
        let value = value.trim();
        add(counts, format!("dom:css:property:{property}"));
        add(counts, format!("dom:css:form:{property}={}", form(value)));
        // Font weight and other short scalar CSS values can distinguish presentation roles.
        if value.len() <= 24
            && value
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || b"_-".contains(&ch))
        {
            add(
                counts,
                format!("dom:css:literal:{property}={}", value.to_ascii_lowercase()),
            );
        }
    }
}

fn prefix(line: &str) -> Option<&'static str> {
    let line = line.trim_start();
    if line.starts_with('¶') {
        return Some("pilcrow");
    }
    if line.starts_with('§') {
        return Some("section-sign");
    }
    if line.starts_with('*') && line.as_bytes().get(1).is_some_and(u8::is_ascii_digit) {
        return Some("star-number");
    }
    let token = line.split_whitespace().next()?;
    let core = token.trim_matches(|ch| matches!(ch, '(' | ')' | '[' | ']' | '.'));
    if core.is_empty() {
        return None;
    }
    if core.bytes().all(|ch| ch.is_ascii_digit() || ch == b'.')
        && core.bytes().any(|ch| ch.is_ascii_digit())
    {
        return Some(if token.starts_with('[') {
            "bracket-number"
        } else if token.starts_with('(') {
            "parenthesized-number"
        } else if core.contains('.') {
            "decimal-sequence"
        } else {
            "number"
        });
    }
    if core.bytes().all(|ch| b"IVXLCDMivxlcdm".contains(&ch)) {
        return Some("roman-form");
    }
    if core.len() == 1 && core.as_bytes()[0].is_ascii_alphabetic() && token != core {
        return Some("letter-marker");
    }
    None
}

fn plain_text(source: &str, counts: &mut Counts) {
    let mut paragraph_lines = 0;
    let mut previous_case = "start";
    for line in source.lines() {
        let trimmed = line.trim();
        add(counts, "text:line");
        if trimmed.is_empty() {
            add(counts, "text:blank-line");
            if paragraph_lines != 0 {
                add(
                    counts,
                    format!("text:paragraph-lines:{}", bucket(paragraph_lines)),
                );
                paragraph_lines = 0;
            }
            previous_case = "blank";
            continue;
        }
        paragraph_lines += 1;
        add(counts, format!("text:line-bytes:{}", bucket(trimmed.len())));
        let indent = line
            .bytes()
            .take_while(|ch| matches!(*ch, b' ' | b'\t'))
            .count();
        add(counts, format!("text:indent-bytes:{}", bucket(indent)));
        if let Some(marker) = prefix(trimmed) {
            add(counts, format!("text:prefix:{marker}"));
        }
        let upper = trimmed.chars().any(char::is_uppercase);
        let lower = trimmed.chars().any(char::is_lowercase);
        let case = match (upper, lower) {
            (true, false) => "upper",
            (false, true) => "lower",
            (true, true) => "mixed",
            _ => "no-letters",
        };
        add(counts, format!("text:case:{case}"));
        add(
            counts,
            format!("text:case-transition:{previous_case}>{case}"),
        );
        previous_case = case;
    }
    if paragraph_lines != 0 {
        add(
            counts,
            format!("text:paragraph-lines:{}", bucket(paragraph_lines)),
        );
    }
    for ch in source.chars() {
        match ch {
            '\u{c}' => add(counts, "text:form-feed"),
            '\r' => add(counts, "text:carriage-return"),
            '\n' => add(counts, "text:line-feed"),
            '\t' => add(counts, "text:tab"),
            '\u{a0}' => add(counts, "text:nbsp"),
            _ => (),
        }
    }
}

pub(super) fn observe(field: &str, source: &str) -> BTreeMap<String, u64> {
    let mut counts = Counts::new();
    if field == "plain_text" {
        plain_text(source, &mut counts);
        return counts.into_iter().collect();
    }
    let dom = Html::parse_fragment(source);
    let root = dom.root_element();
    let mut contexts = HashMap::new();
    let mut previous_children = HashMap::new();
    let mut targets = HashMap::new();
    let mut scoped_targets = HashMap::<Option<usize>, HashMap<&str, Target<'_>>>::new();
    let mut other_names = HashSet::new();
    let mut links = Vec::new();
    let mut ordinal = 0;
    let empty = Context {
        tag: "root",
        parent_tag: "root",
        opinion: None,
        ancestor_cues: 0,
        own_cues: 0,
    };
    for node in root.descendants() {
        let parent = node
            .parent()
            .and_then(|p| contexts.get(&p.id()).copied())
            .unwrap_or(empty);
        let Some(element) = node.value().as_element() else {
            if let Node::Text(text) = node.value() {
                if let Some(marker) = prefix(text) {
                    add(
                        &mut counts,
                        format!("dom:text-prefix:{marker}@{}", parent.tag),
                    );
                }
            }
            if matches!(node.value(), Node::Fragment) {
                contexts.insert(node.id(), parent);
            }
            continue;
        };
        let tag = element.name();
        let mut own = cues(tag);
        let mut opinion_declaration = own & 1 != 0;
        add(&mut counts, format!("dom:tag:{tag}"));
        add(&mut counts, format!("dom:namespace:{}", element.name.ns));
        add(&mut counts, format!("dom:parent:{}>{tag}", parent.tag));
        add(
            &mut counts,
            format!("dom:path:{}>{}>{tag}", parent.parent_tag, parent.tag),
        );
        if let Some(parent_node) = node.parent() {
            if let Some(previous) = previous_children.insert(parent_node.id(), tag) {
                add(&mut counts, format!("dom:siblings:{previous}>{tag}"));
            }
        }
        for (name, value) in &element.attrs {
            let local = name.local.as_ref();
            let qualified = name
                .prefix
                .as_ref()
                .map_or_else(|| local.to_owned(), |prefix| format!("{prefix}:{local}"));
            let value: &str = value.as_ref();
            add(&mut counts, format!("dom:attribute:{qualified}"));
            add(&mut counts, format!("dom:attribute-on:{tag}@{qualified}"));
            add(
                &mut counts,
                format!("dom:value-shape:{qualified}={}", value_shape(value)),
            );
            if local == "class" {
                for class in value.split_ascii_whitespace() {
                    let bits = cues(class);
                    own |= bits;
                    opinion_declaration |= bits & 1 != 0;
                    add(&mut counts, format!("dom:class-form:{}", form(class)));
                }
            } else if local == "style" {
                css(&mut counts, value);
            } else if matches!(local, "id" | "name" | "eid") {
                own |= cues(value);
                add(
                    &mut counts,
                    format!("dom:identifier:{qualified}={}", identifier(value)),
                );
            } else if local == "type" || local == "role" || local.starts_with("data-") {
                let bits = cues(value);
                own |= bits;
                opinion_declaration |= bits & 1 != 0;
            }
            let structural_literal = matches!(
                local,
                "type" | "role" | "data-type" | "align" | "dir" | "reversed"
            ) || qualified == "epub:type"
                || qualified == "xmlns"
                || qualified.starts_with("xmlns:");
            let short_atom = value.len() <= 24
                && value
                    .bytes()
                    .all(|ch| ch.is_ascii_alphabetic() || b"_-".contains(&ch))
                && !instance_text_attribute(local)
                && !matches!(
                    local,
                    "id" | "name"
                        | "eid"
                        | "class"
                        | "style"
                        | "href"
                        | "src"
                        | "title"
                        | "alt"
                        | "aria-label"
                );
            if structural_literal && value.len() <= 160 {
                add(
                    &mut counts,
                    format!("dom:value-literal:{qualified}={value}"),
                );
            } else if short_atom {
                add(&mut counts, format!("dom:value-atom:{qualified}={value}"));
            }
            if instance_text_attribute(local) {
                add(
                    &mut counts,
                    format!(
                        "dom:instance-value-bytes:{qualified}={}",
                        bucket(value.len())
                    ),
                );
            }
            if local.starts_with("data-") && !matches!(local, "data-id" | "data-url" | "data-href")
            {
                add(
                    &mut counts,
                    format!("dom:data-form:{qualified}={}", form(value)),
                );
            }
            if matches!(local, "label" | "start" | "value" | "rowspan" | "colspan") {
                add(
                    &mut counts,
                    format!("dom:counter-form:{qualified}={}", form(value)),
                );
            }
        }
        let opinion = if opinion_declaration {
            Some(ordinal)
        } else {
            parent.opinion
        };
        let context = Context {
            tag,
            parent_tag: parent.tag,
            opinion,
            ancestor_cues: parent.ancestor_cues | parent.own_cues,
            own_cues: own,
        };
        for (i, cue) in CUES.iter().enumerate() {
            if own & (1 << i) != 0 {
                add(&mut counts, format!("cue:{cue}@{tag}"));
                for (j, ancestor) in CUES.iter().enumerate() {
                    if context.ancestor_cues & (1 << j) != 0 {
                        add(&mut counts, format!("cue:ancestor:{ancestor}>{cue}"));
                    }
                }
            }
        }
        let id = element.attr("id").unwrap_or_default();
        let name = element.attr("name").unwrap_or_default();
        for value in [id, if tag == "a" && name != id { name } else { "" }] {
            if !value.is_empty() {
                target(&mut targets, value, tag, own | context.ancestor_cues);
                target(
                    scoped_targets.entry(opinion).or_default(),
                    value,
                    tag,
                    own | context.ancestor_cues,
                );
            }
        }
        if tag != "a" && !name.is_empty() {
            other_names.insert(name);
        }
        if let Some(href) = element.attr("href") {
            let href = href.trim();
            if let Some(fragment) = href.strip_prefix('#') {
                links.push((tag, opinion, fragment, own | context.ancestor_cues));
                add(
                    &mut counts,
                    format!("dom:href-local-scheme:{}", identifier(fragment)),
                );
                if fragment.contains('%') {
                    add(&mut counts, "dom:href-local:percent-encoded");
                }
            } else {
                let kind = if href.starts_with("//") {
                    "network-path"
                } else if href.contains("://") {
                    "absolute-url"
                } else if href.contains(':') {
                    "scheme"
                } else if href.contains('#') {
                    "relative-with-fragment"
                } else {
                    "relative"
                };
                add(&mut counts, format!("dom:href:{kind}"));
            }
        }
        contexts.insert(node.id(), context);
        ordinal += 1;
    }
    for (tag, owner, fragment, source_cues) in links {
        add(&mut counts, format!("dom:local-link-from:{tag}"));
        if fragment.is_empty() {
            add(&mut counts, "dom:local-link:empty-fragment");
            continue;
        }
        let destination = targets.get(fragment);
        let status = match destination {
            None => "missing",
            Some(t) if t.count == 1 => "unique",
            Some(_) => "duplicate",
        };
        add(&mut counts, format!("dom:local-link:{status}"));
        if let Some(t) = destination.filter(|t| t.count == 1) {
            add(&mut counts, format!("dom:local-link-tags:{tag}>{}", t.tag));
            for (i, from) in CUES.iter().enumerate() {
                if source_cues & (1 << i) != 0 {
                    for (j, to) in CUES.iter().enumerate() {
                        if t.cues & (1 << j) != 0 {
                            add(&mut counts, format!("cue:local-link:{from}>{to}"));
                        }
                    }
                }
            }
        }
        let scoped = scoped_targets
            .get(&owner)
            .and_then(|scope| scope.get(fragment));
        let scope_status = match scoped {
            None => "missing",
            Some(t) if t.count == 1 => "unique",
            Some(_) => "duplicate",
        };
        add(
            &mut counts,
            format!(
                "cue:local-link-scope:{}:{scope_status}",
                if owner.is_some() {
                    "opinion"
                } else {
                    "unscoped"
                }
            ),
        );
        if destination.is_some() && scoped.is_none() {
            add(&mut counts, "cue:local-link:foreign-opinion-scope");
        }
        if destination.is_none() && other_names.contains(fragment) {
            add(&mut counts, "dom:local-link:non-a-name-only");
        }
    }
    for destination in targets.values() {
        if destination.count > 1 {
            add(
                &mut counts,
                format!("dom:duplicate-target:{}", bucket(destination.count)),
            );
        }
    }
    counts.into_iter().collect()
}
