//! A document's enumerated headings read as one ladder of counters ("I.", "A.", "1."), each
//! enumerator in the families it can belong to: shared by the PDF parser's heading reading and
//! the structure engine's note pairing, which must not take an outline's numbers for notes.

use regex::Regex;
use std::collections::BTreeMap;
use std::sync::OnceLock;

const MAX_COUNTER_VALUE: u32 = 200;
/// Legal outlines run six deep: PART I, A., 1., (a), (i), (A).
const MAX_OUTLINE_DEPTH: usize = 6;
const FOOTNOTE_SUSPECT_MIN_VALUE: u32 = 15;

fn roman_to_int(value: &str) -> Option<u32> {
    legal_layout::roman_value(value).filter(|number| *number <= MAX_COUNTER_VALUE)
}

pub type EnumeratorInterpretation = (String, u32, usize);

pub fn enumerator_interpretations(value: &str, punct: &str) -> Vec<EnumeratorInterpretation> {
    static LEGAL_NUMERIC: OnceLock<Regex> = OnceLock::new();
    static ROMAN: OnceLock<Regex> = OnceLock::new();
    let mut result = Vec::new();
    if LEGAL_NUMERIC
        .get_or_init(|| Regex::new(r"^\d{1,2}(?:\.\d{1,2}){1,3}$").unwrap())
        .is_match(value)
    {
        let parts = value.split('.').collect::<Vec<_>>();
        if let Some(tail) = parts
            .last()
            .and_then(|part| part.parse::<u32>().ok())
            .filter(|number| (1..=MAX_COUNTER_VALUE).contains(number))
        {
            result.push((format!("legal_numeric_{punct}"), tail, parts.len()));
        }
        return result;
    }
    if value.chars().all(|character| character.is_ascii_digit()) {
        if let Some(number) = value
            .parse::<u32>()
            .ok()
            .filter(|number| (1..=MAX_COUNTER_VALUE).contains(number))
        {
            result.push((format!("numeric_{punct}"), number, 0));
        }
        return result;
    }
    let upper = value.to_uppercase();
    // Small numerals count a family of their own ("(i)", "(ii)" under "(a)").
    let roman_family = if value == upper { "roman" } else { "lower_roman" };
    if (value == upper || value == value.to_lowercase())
        && ROMAN
            .get_or_init(|| Regex::new(r"^[IVXLCDM]{2,7}$").unwrap())
            .is_match(&upper)
    {
        if let Some(number) = roman_to_int(value) {
            result.push((format!("{roman_family}_{punct}"), number, 0));
        }
        return result;
    }
    if value.chars().count() == 1 && value.chars().all(char::is_alphabetic) {
        let family = if value.chars().all(char::is_uppercase) {
            "upper_alpha"
        } else {
            "lower_alpha"
        };
        if "IVXLCDM".contains(&upper) {
            if let Some(number) = roman_to_int(value) {
                result.push((format!("{roman_family}_{punct}"), number, 0));
            }
        }
        let character = upper.chars().next().unwrap();
        let number = u32::from(character).wrapping_sub(u32::from('A')) + 1;
        result.push((format!("{family}_{punct}"), number, 0));
    }
    result
}

#[derive(Clone, Copy)]
struct Frame<'a> {
    family: &'a str,
    value: u32,
}

#[derive(Debug, Default)]
pub struct HeadingFamilyStats {
    pub count: usize,
    pub max_value: u32,
    pub level_votes: BTreeMap<usize, usize>,
    pub violations: usize,
    pub gaps: usize,
    pub footnote_suspect: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeadingAction {
    Increment,
    OpenLevel,
    IllegalRestart,
    JumpForward,
    OpenMidcounter,
    Violation,
}

#[derive(Debug)]
pub struct HeadingAssignment<'a> {
    pub family: &'a str,
    pub value: Option<u32>,
    pub level: Option<usize>,
    pub action: HeadingAction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeadingLadderStatus {
    NoEnumerators,
    ParsedClean,
    ParsedWithViolations,
    Unparseable,
}

#[derive(Debug)]
pub struct HeadingLadder<'a> {
    pub assignments: Vec<HeadingAssignment<'a>>,
    pub violations: usize,
    pub gaps: usize,
    pub families: BTreeMap<&'a str, HeadingFamilyStats>,
    pub status: HeadingLadderStatus,
}

pub fn parse_heading_ladder<'a>(
    candidates: impl IntoIterator<Item = &'a [EnumeratorInterpretation]>,
) -> HeadingLadder<'a> {
    let mut stack: Vec<Frame<'a>> = Vec::new();
    let mut assignments = Vec::new();
    let mut families: BTreeMap<&'a str, HeadingFamilyStats> = BTreeMap::new();
    let mut violations = 0_usize;
    let mut gaps = 0_usize;
    let mut enumerator_count = 0_usize;

    for choices in candidates {
        enumerator_count += 1;
        let mut chosen: Option<(&'a str, u32, HeadingAction, usize)> = None;
        for (family, value, _) in choices {
            if let Some(index) = stack.iter().rposition(|frame| frame.family == family) {
                if stack[index].value + 1 == *value {
                    stack.truncate(index + 1);
                    stack[index].value = *value;
                    chosen = Some((family, *value, HeadingAction::Increment, index + 1));
                    break;
                }
            }
        }
        if chosen.is_none() {
            for (family, value, _) in choices {
                if *value == 1
                    && !stack.iter().any(|frame| frame.family == family)
                    && stack.len() < MAX_OUTLINE_DEPTH
                {
                    stack.push(Frame { family, value: 1 });
                    chosen = Some((family, *value, HeadingAction::OpenLevel, stack.len()));
                    break;
                }
            }
        }
        if chosen.is_none() {
            // A skipped number is the shortest jump that reads it: "C." after "A." is the
            // third letter, not the Roman hundred.
            let mut jump: Option<(&'a str, u32, usize, u32)> = None;
            for (family, value, _) in choices {
                if let Some(index) = stack.iter().rposition(|frame| frame.family == family) {
                    if *value == 1 && jump.is_none() {
                        stack.truncate(index + 1);
                        stack[index].value = 1;
                        chosen = Some((family, *value, HeadingAction::IllegalRestart, index + 1));
                        break;
                    }
                    let gap = value.saturating_sub(stack[index].value);
                    if gap > 1 && jump.is_none_or(|(.., shortest)| gap < shortest) {
                        jump = Some((family, *value, index, gap));
                    }
                }
            }
            if let Some((family, value, index, _)) = jump.filter(|_| chosen.is_none()) {
                stack.truncate(index + 1);
                stack[index].value = value;
                chosen = Some((family, value, HeadingAction::JumpForward, index + 1));
            }
        }
        if chosen.is_none() {
            for (family, value, _) in choices {
                if !stack.iter().any(|frame| frame.family == family)
                    && stack.len() < MAX_OUTLINE_DEPTH
                {
                    stack.push(Frame {
                        family,
                        value: *value,
                    });
                    chosen = Some((family, *value, HeadingAction::OpenMidcounter, stack.len()));
                    break;
                }
            }
        }
        let Some((family, value, action, level)) = chosen else {
            let (family, value) = choices
                .first()
                .map(|(family, value, _)| (family.as_str(), Some(*value)))
                .unwrap_or(("unknown", None));
            violations += 1;
            families.entry(family).or_default().violations += 1;
            assignments.push(HeadingAssignment {
                family,
                value,
                level: None,
                action: HeadingAction::Violation,
            });
            continue;
        };
        let family_stats = families.entry(family).or_default();
        if action == HeadingAction::IllegalRestart {
            violations += 1;
            family_stats.violations += 1;
        } else if matches!(
            action,
            HeadingAction::JumpForward | HeadingAction::OpenMidcounter
        ) {
            gaps += 1;
            family_stats.gaps += 1;
        }
        family_stats.count += 1;
        family_stats.max_value = family_stats.max_value.max(value);
        *family_stats.level_votes.entry(level).or_default() += 1;
        assignments.push(HeadingAssignment {
            family,
            value: Some(value),
            level: Some(level),
            action,
        });
    }

    for (family, stats) in &mut families {
        stats.footnote_suspect = (family.starts_with("numeric_")
            || family.starts_with("legal_numeric_"))
            && stats.max_value >= FOOTNOTE_SUSPECT_MIN_VALUE
            && stats.level_votes.len() == 1;
    }
    let status = if enumerator_count == 0 {
        HeadingLadderStatus::NoEnumerators
    } else if violations == 0 {
        HeadingLadderStatus::ParsedClean
    } else if violations <= (enumerator_count / 5).max(1) {
        HeadingLadderStatus::ParsedWithViolations
    } else {
        HeadingLadderStatus::Unparseable
    };
    HeadingLadder {
        assignments,
        violations,
        gaps,
        families,
        status,
    }
}
