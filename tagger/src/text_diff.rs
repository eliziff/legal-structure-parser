//! Myers diff from diff-match-patch, in the Unicode scalar coordinates used by the gold.
use diff_match_patch_rs::{Compat, DiffMatchPatch, Ops};

pub fn matching_blocks(a: &[char], b: &[char]) -> Result<Vec<(usize, usize, usize)>, String> {
    if a == b {
        return Ok(vec![(0, 0, a.len())]);
    }
    let mut diff = DiffMatchPatch::default();
    // Scoring must be deterministic and independent of OCR line segmentation.
    diff.set_timeout(None);
    diff.set_checklines(false);
    let changes = diff
        .diff_main::<Compat>(&a.iter().collect::<String>(), &b.iter().collect::<String>())
        .map_err(|e| format!("Text alignment: {e:?}"))?;
    let (mut left, mut right) = (0, 0);
    let mut blocks = Vec::new();
    for change in changes {
        match change.op() {
            Ops::Equal => {
                blocks.push((left, right, change.size()));
                left += change.size();
                right += change.size();
            }
            Ops::Delete => left += change.size(),
            Ops::Insert => right += change.size(),
        }
    }
    Ok(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_offsets_are_unicode_scalars() {
        let a: Vec<char> = "A 🦉 café!".chars().collect();
        let b: Vec<char> = "A 🦉 cafes!".chars().collect();
        assert_eq!(matching_blocks(&a, &b).unwrap(), vec![(0, 0, 7), (8, 9, 1)]);
        assert!(matching_blocks(&a, &[]).unwrap().is_empty());
    }
}
