//! A judgment's author line, which opens its reasons, and the signature lines that close them.
use regex::Regex;
use std::sync::OnceLock;

/// A judge's or member's signature, or a release line, as a decision prints them after its reasons
/// ("“K. Feldman J.A.”", "“I agree. L.B. Roberts J.A.”", "Bateman, J.A.", "MacDonald, C.J.N.S.", "Smith J.S.C.",
/// "Released: November 28, 2022").
pub fn signature_line(value: &str) -> bool {
    static SIGNATURE: OnceLock<Regex> = OnceLock::new();
    static RELEASE: OnceLock<Regex> = OnceLock::new();
    let value = value.trim().trim_matches(['"', '“', '”', '\'', '‘', '’']).trim();
    !value.is_empty()
        && value.chars().count() <= 90
        && (SIGNATURE
            .get_or_init(|| {
                Regex::new(r"\b(?:J|JJ|C\.?J|C\.?J\.?C|C\.?J\.?O|C\.?J\.?N\.?S|J\.?A|JJ\.?A|J\.?A\.?N\.?S|J\.?C\.?A|J\.?S\.?C|Q\.?C|K\.?C)\.(?:\s*\((?i:ad hoc|dissenting|concurring)\))?:?$|^L[ae] juge \p{Lu}[\p{L} .'’-]*(?:\s*\((?i:ad hoc|dissidente?)\))?$|\b(?:JJ?A|CJ[A-Z]{0,2})$|\b(?:Member|Adjudicator|Arbitrator|Chairperson|Vice-Chairperson|Vice-Chair|Commissioner|Associate Judge|Master|Prothonotary|Referee)$|^(?:Vice-)?[Pp]r[ée]sident(?:e)?$|^Vice-[Pp]resident$")
                    .expect("signature regex")
            })
            .is_match(value)
            || RELEASE
                .get_or_init(|| Regex::new(r"^(?i:released|date of release|dated)\b|^(?i:i agree)(?:[.:,;]|\s*$)").expect("release regex"))
                .is_match(value))
}

/// A date alone on its line, a place before it, as a decision is dated under its reasons: "September 25, 2007", "Ottawa,
/// December 30, 1997", "Montréal, le 3 mai 2012".
pub fn date_line(value: &str) -> bool {
    static DATE: OnceLock<Regex> = OnceLock::new();
    DATE.get_or_init(|| {
        Regex::new(r"^(?:\p{Lu}[\p{L}'’. -]{2,30},\s*)?(?:le\s+)?(?:(?:January|February|March|April|May|June|July|August|September|October|November|December)\s+\d{1,2}(?:st|nd|rd|th)?,?\s+\d{4}|\d{1,2}(?:er)?\s+(?:janvier|février|mars|avril|mai|juin|juillet|août|septembre|octobre|novembre|décembre)\s+\d{4})\.?$").expect("date regex")
    })
    .is_match(value.trim())
}

/// A figure's caption alone on its line ("Figure 6."): it and what the figure prints under it are no paragraph's words.
pub fn figure_caption(line: &str) -> bool {
    let mut words = line.split_whitespace();
    words.next() == Some("Figure") && words.next().is_some_and(|number| number.trim_end_matches('.').bytes().all(|byte| byte.is_ascii_digit())) && words.next().is_none()
}

/// A line opening what stands after a judge's reasons, in either language: the formal judgment ("FOR THESE REASONS, THE
/// COURT:", "JUDGMENT in IMM-15723-24", "THIS COURT'S JUDGMENT is that:", "POUR CES MOTIFS"), the record of the hearing
/// ("Appeal heard on May 2, 2017", "Memorandum filed at Edmonton"), counsel ("SOLICITORS OF RECORD:", "APPEARANCES:",
/// "NAMES OF COUNSEL", "Counsel for the first plaintiff: Martin Hayden SC", "Counsel for the Petitioners I.H. Fraser"; not
/// "Counsel for the parties agreed."), the concurrence ("Concurred in:"), a signature ("Signed:", "- Sgd -", "“Hon Brendan
/// Brown KC”", "The Honourable Madam Justice Forth") or an appendix
/// ("Appendix A – Proceeding participants"). A short line: a sentence that merely begins so is the reasons'.
pub fn end_matter(value: &str) -> bool {
    static CAPITALS: OnceLock<Regex> = OnceLock::new();
    static OPENING: OnceLock<Regex> = OnceLock::new();
    let value = value.trim().trim_matches(['"', '“', '”']);
    value.split_whitespace().count() <= 24
        && (CAPITALS
            .get_or_init(|| Regex::new(r"^(?:FOR (?:THESE|THOSE) REASONS|POUR CES MOTIFS|JUDGMENT\b|JUGEMENT\b|THIS COURT['’]S JUDGMENT|APPENDIX\b|ANNEXE\b|SCHEDULE\b|FEDERAL COURT\b)").expect("capitals regex"))
            .is_match(value)
            || OPENING
                .get_or_init(|| Regex::new(r"(?i)^(?:-\s*)?(?:appeal heard|heard (?:on|at)\b|applications? heard|memorandum filed|reasons filed|issued on\b|signed\b|sgd\b|dated (?:at|on|in|this)\b|counsel for\b(?:[^:]*:|(?:\s+the)?\s+\S+(?:\s+(?:and|(?-i:\p{Lu})\S*))*$)|dated:|released:|i agree\.?$|(?:the )?hon(?:ou?rable)?\b|solicitors of record|names of counsel|appearances\b|written representations|concurred in\b|appendix [a-z0-9]\b|annexe [a-z0-9]\b|solicitors? for\b|procureurs? d)").expect("opening regex"))
                .is_match(value))
}

/// A report's line recording what became of the proceeding, under the reasons: "Appeal dismissed with costs.", "Pourvoi rejeté".
pub fn disposition_line(value: &str) -> bool {
    const DISPOSITIONS: [&str; 11] = ["Appeal allowed", "Appeal dismissed", "Appeals allowed", "Appeals dismissed", "Cross-appeal", "Pourvoi accueilli", "Pourvoi rejet", "Pourvois accueillis", "Pourvois rejet", "Pourvoi principal", "Pourvoi et pourvois"];
    // (As the page prints it: "Pourvois\u{a0}rejetés", "cross\u{2011}appeals".)
    let printed = value.contains(['\u{a0}', '\u{2011}']).then(|| value.replace('\u{a0}', " ").replace('\u{2011}', "-"));
    let value = printed.as_deref().unwrap_or(value);
    static NAMED: OnceLock<Regex> = OnceLock::new();
    // (Or names its party or what it disposes of: "East's appeal dismissed.", "Pourvoi de Javid Ahmad rejeté.", "Judgment accordingly.", "Motions dismissed.",
    // "Motions for extension of time to serve and file notices of appeal dismissed.", "Application dismissed.", "Requête accordée.", "Les requêtes en prorogation de délai ... sont rejetés.")
    let named = NAMED.get_or_init(|| Regex::new(r"^(?:\p{Lu}[\p{L}.'’-]*(?:\s\p{Lu}[\p{L}.'’-]*){0,3}['’]s (?:cross-)?appeals? (?:dismissed|allowed)|Appeal and cross-appeals\b|Pourvois? d(?:e |u |['’]).{0,60}(?:rejet|accueilli)|Judgment accordingly|Jugement en cons[ée]quence|(?:Motions?|Applications?) (?:.{0,160} )?(?:dismissed|granted|refused)\.?$|(?:Demandes?|Requ.{1,2}tes?|Les (?:demandes|requ.{1,2}tes)) (?:.{0,160} )?(?:rejet|accueilli|accord)\S*\.?$)").expect("disposition regex"));
    DISPOSITIONS.iter().any(|opening| value.trim_start().starts_with(opening)) || named.is_match(value.trim_start())
}

/// A sentence stating what the court does with the proceeding, in either language: "The appeal is, therefore, allowed
/// …", "I would therefore allow the appeal …", "… dismissed with costs", "… a verdict of acquittal is substituted",
/// "Le pourvoi est accueilli".
pub fn disposition(value: &str) -> bool {
    static PROCEEDING: OnceLock<Regex> = OnceLock::new();
    static ORDER: OnceLock<Regex> = OnceLock::new();
    PROCEEDING
        .get_or_init(|| Regex::new(r"(?i)\b(?:cross-)?(?:appeals?|applications?|motions?|petitions?|actions?|pourvois?|appels?|requêtes?|demandes?)\b").expect("proceeding regex"))
        .is_match(value)
        && ORDER
            .get_or_init(|| Regex::new(r"(?i)\b(?:allow|dismiss|grant|den(?:y|ied)|quash|accueill|rejet|rejeté|substitut)").expect("order regex"))
            .is_match(value)
        || value.to_lowercase().contains("costs") || value.to_lowercase().contains("dépens") || value.to_lowercase().contains("acquittal")
}

/// One decretal act: the participle a court's order is passive in, and the verb it speaks with.
const DECRETAL: [(&str, &str); 26] = [("rendered", "render"), ("absolute", "set"), ("error", "set"), ("allowed", "set"), ("refused", "set"), ("suspended", "suspend"), ("stricken", "strike"), ("disbarred", "disbar"), ("affirmed", "affirm"), ("reversed", "reverse"), ("vacated", "vacate"), ("remanded", "remand"), ("modified", "modify"), ("quashed", "quash"),
    ("dismissed", "dismiss"), ("denied", "deny"), ("granted", "grant"), ("sustained", "sustain"), ("overruled", "overrule"), ("confirmed", "confirm"), ("discharged", "discharge"),
    ("upheld", "uphold"), ("enjoined", "enjoin"), ("disallowed", "disallow"), ("reinstated", "reinstate"), ("aside", "set")];

/// The acts a sentence of a US opinion orders, by the words that order them, and whether it is an order at all: passive
/// ("the judgment is affirmed", "the objection will be sustained"), spoken ("We reverse.", "I will grant the prayer.", "the
/// Court denies"), a formula ("IT IS THEREFORE, BY THE COURT, ORDERED", "SO ORDERED"), a judgment to come ("An appropriate
/// order will be entered.") or a bare opening ("Reversed and remanded to the Island Court.", "Judgment for the plaintiff.").
/// What a court says generally, of another case or of a party's contention, or in a separate opinion's "would", orders
/// nothing. A sentence that orders without naming an act is `ORDERED` or `JUDGMENT`: "SO ORDERED.", "Decree for plaintiff."
pub const ORDERED: &str = "ordered";
pub const JUDGMENT: &str = "judgment";

pub fn court_order(value: &str) -> Option<Vec<&'static str>> {
    const SAID: [&str; 15] = ["held", "holding", "holds", "insisting", "contends", "contend", "argues", "argued", "requested", "requests", "asks", "asked", "seeks", "urges", "prays"];
    const CONDITIONAL: [&str; 10] = ["would", "if", "unless", "whether", "when", "where", "once", "after", "because", "although"];
    const DOUBTFUL: [&str; 7] = ["may", "might", "can", "could", "cannot", "not", "ordinarily"];
    const AUXILIARY: [&str; 14] = ["will", "shall", "must", "do", "does", "hereby", "therefore", "accordingly", "thus", "then", "feel", "compelled", "are", "am"];
    const RULED: [&str; 21] = ["judgment", "judgments", "decree", "order", "decision", "conviction", "report", "petition", "motion", "rule", "appeal", "application", "writ", "bill", "complaint", "exceptions", "action", "case", "claim", "prayer", "request"];
    let value = value.trim().trim_matches(['"', '“', '”']);
    let lowered = value.to_lowercase();
    // The words, and the clause each stands in: "if" and "would" speak for their own clause.
    let (mut words, mut clauses, mut clause) = (Vec::new(), Vec::new(), 0);
    let mut opened = None;
    for (index, c) in lowered.char_indices().chain(std::iter::once((lowered.len(), ' '))) {
        if c.is_alphabetic() {
            opened.get_or_insert(index);
        } else {
            if let Some(start) = opened.take() {
                words.push(&lowered[start..index]);
                clauses.push(clause);
            }
            clause += usize::from(matches!(c, ',' | ';' | ':'));
            // "May 6, 1983" is a month, no doubt of anything.
            if c.is_ascii_digit() && lowered[..index].trim_end().ends_with("may") {
                if let Some(word) = words.last_mut() { *word = "month"; }
            }
        }
    }
    // What a citation leaves of "cert. denied, 444 U.S. 1"; "$7,400." and "$529,616.68 is owed" are money.
    let tail = value.rsplit(',').next().unwrap_or("");
    let page_last = tail.split_whitespace().skip(1).any(|word| word.starts_with(|c: char| c.is_ascii_digit() || c == '('));
    let cited = value.contains(',') && tail.trim_start().starts_with(|c: char| c.is_ascii_digit()) && tail.contains(char::is_uppercase) && page_last;
    if words.is_empty() || cited || value.starts_with('(') || words.starts_with(&["in", "order"]) {
        return None;
    }
    let reported = |at: usize| (0..at).any(|before| SAID.contains(&words[before]) || clauses[before] == clauses[at] && CONDITIONAL.contains(&words[before]));
    let act_at = |word: &str| DECRETAL.iter().position(|act| act.0 == word);
    let mut acts = Vec::new();
    for (at, word) in words.iter().enumerate() {
        let before = &words[at.saturating_sub(5)..at];
        // Passive: a form of "be" before the participle, and no doubt between them.
        if let Some(act) = act_at(word) {
            let phrasal = match *word {
                "aside" => before.last() == Some(&"set"),
                "absolute" => before.last() == Some(&"made"),
                "error" => before.ends_with(&["there", "is", "no"]) && at + 1 == words.len(),
                _ => true,
            };
            // "allowed" and "suspended" are any sentence's words: an order only of what is before the court.
            let plain_word = matches!(*word, "allowed" | "refused" | "suspended" | "stricken");
            let ruling = !plain_word || before.iter().any(|near| RULED.contains(near) || *near == "hereby");
            let verb = before.iter().rposition(|near| matches!(*near, "is" | "are" | "be" | "stand" | "stands"));
            // Doubt reaches the verb back to the clause's "and": "may only be granted", not "not well taken and is denied".
            let doubted = |verb: usize| before[verb..].iter().any(|near| DOUBTFUL.contains(near) && (*word != "error" || *near != "not"))
                || before[..verb].iter().rev().take_while(|near| !matches!(**near, "and" | "or")).any(|near| DOUBTFUL.contains(near));
            let general = matches!(words[0], "a" | "an") && (at + 1..words.len()).take_while(|after| clauses[*after] == clauses[at]).any(|after| matches!(words[after], "when" | "if" | "where" | "unless" | "only"));
            if phrasal && ruling && !general && !reported(at) && verb.is_some_and(|verb| !doubted(verb)) {
                acts.push(act);
            }
            // A bare opening: the word itself, or a ruling's name before it, or closing a sentence that opens on one.
            if phrasal && (at == 0 || RULED.contains(&words[0]) && (at <= 2 || at + 1 == words.len())) {
                acts.push(act);
            }
        }
        // Spoken: the court, with what helps its verb between.
        if matches!(*word, "we" | "i" | "court" | "board") && !reported(at) {
            let spoken = words[at + 1..].iter().take_while(|next| AUXILIARY.contains(next) || *next == &"to").count();
            if let Some(next) = words.get(at + 1 + spoken) {
                let stem = next.strip_suffix("ies").map(|stem| format!("{stem}y")).or_else(|| next.strip_suffix("es").filter(|stem| stem.ends_with("ss") || stem.ends_with('h')).map(str::to_owned))
                    .or_else(|| next.strip_suffix('s').filter(|stem| !stem.ends_with('s')).map(str::to_owned)).unwrap_or_else(|| (*next).to_owned());
                if let Some(act) = DECRETAL.iter().position(|act| act.1 == stem && act.1 != "set") {
                    acts.push(act);
                }
            }
        }
    }
    let formula = words.windows(2).position(|pair| pair == ["so", "ordered"]).is_some_and(|at| !reported(at))
        || matches!(words[0], "ordered" | "adjudged" | "decreed")
        || words.windows(2).position(|pair| pair == ["it", "is"]).is_some_and(|at| !reported(at) && words[at + 2..(at + 9).min(words.len())].iter().any(|word| matches!(*word, "ordered" | "adjudged" | "decreed")));
    // What is to be settled: "A decree may be prepared.", "Counsel will prepare findings."
    let settled = |at: usize, word: &&str| matches!(*word, "entered" | "issue" | "signed" | "drawn" | "prepared" | "prepare" | "submit" | "submitted" | "settled")
        && at > 0 && matches!(words[at - 1], "will" | "shall" | "may" | "must" | "be") && !reported(at)
        && !matches!(words[at.saturating_sub(2)], "can" | "could" | "cannot")
        && (at.saturating_sub(6)..at).any(|near| matches!(words[near], "order" | "decree" | "judgment" | "findings") && !(near > 0 && matches!(words[near - 1], "our" | "my")));
    let to_come = words.iter().enumerate().any(|(at, word)| settled(at, word))
        || words.windows(3).any(|run| run[0] == "let" && matches!(run[1], "a" | "an" | "the" | "judgment" | "decree" | "there"))
        || words.len() >= 3 && matches!(words[0], "judgment" | "decree" | "verdict") && words[1] == "for";
    // Relief named as a party's: "take nothing", "have and recover", a finding of guilt.
    let relief = words.windows(2).any(|pair| pair == ["take", "nothing"]) || words.windows(3).any(|run| run == ["have", "and", "recover"])
        || words.iter().enumerate().any(|(at, word)| *word == "guilty" && !reported(at) && words[at.saturating_sub(4)..at].iter().any(|near| matches!(*near, "is" | "are" | "finds")));
    if acts.is_empty() && !formula && !to_come && !relief {
        return None;
    }
    // An order's terms, as a participle or as what is to be entered: "order will be entered, dismissing the bill".
    if formula || to_come {
        acts.extend(words.iter().filter_map(|word| {
            let gerund = word.strip_suffix("ing")?;
            DECRETAL.iter().position(|act| act.1 != "set" && act.1.trim_end_matches('e') == gerund)
        }));
    }
    if formula {
        acts.extend(words.iter().filter_map(|word| act_at(word).filter(|act| !matches!(DECRETAL[*act].0, "aside" | "absolute" | "error"))));
    }
    acts.sort_unstable();
    acts.dedup();
    let mut named = acts.into_iter().map(|act| match DECRETAL[act].0 { "aside" => "set aside", "absolute" => "made absolute", "error" => "no error", act => act }).collect::<Vec<_>>();
    if named.is_empty() {
        named.push(if formula { ORDERED } else { JUDGMENT });
    }
    Some(named)
}

/// The line a decision's reasons open with, naming who gives them: a judge's name and title before
/// a dash ("MARTLAND J.—…", "LE JUGE SOPINKA — …", "LE JUGE MCLACHLIN (dissidente en partie) —", "La juge Charron —", "Le Juge
/// en chef et le juge Major —"), "The judgment of the Court was delivered by", "The reasons for judgment
/// were delivered by", "The reasons of Bastarache and Rothstein JJ. were delivered by", "Version française des motifs".
pub fn author_line(line: &str) -> bool {
    static AUTHOR: OnceLock<Regex> = OnceLock::new();
    AUTHOR
        .get_or_init(|| {
            Regex::new(r"^[A-Z][A-Za-zÀ-ÿ'’´. -]{1,60}?(?:C\.J\.[A-Z.]*|J\.A\.|JJ\.|J\.)\s*(?:[—–]\s*|-\s+|-\s*$)(?:\S|$)|^L(?:[AEae]|ES|es)\s+(?i:juges?)(?:\s+(?:(?i:en|chef|et|l[ae]s?|juges?)\b|\p{Lu}[\p{L}'’´.-]*|\([^)]*\)))*\s*(?:[—–]\s*|--\s*)(?:\S|$)").expect("author regex")
        })
        .is_match(line)
        || line.contains("judgment of the Court was delivered by")
        || line.contains("reasons for judgment were delivered by")
        || (line.starts_with("The reasons of ") || line.starts_with("The judgment of ")) && (line.contains("delivered by") || line.contains(" C.J.") || line.contains(" J.") || line.contains(" JJ."))
        || line.starts_with("Version fran")
        || line.starts_with("The following are the reasons delivered by")
        || line.starts_with("Les motifs suivants ont été rendus par")
        || line.starts_with("English version of the")
}
