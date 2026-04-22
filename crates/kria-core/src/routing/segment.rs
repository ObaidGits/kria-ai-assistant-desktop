//! Multi-intent segmenter.
//!
//! Splits a prompt into sub-prompts when the user expresses multiple commands
//! in one utterance. Only activates when ≥2 imperative verb tokens are found
//! (gated by the verb classifier to avoid over-splitting).

use once_cell::sync::Lazy;
use regex::Regex;

/// Split separators — conjunctions and punctuation that join distinct commands.
static SPLIT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\s+(?:and(?:\s+then)?|then|also|after\s+that|additionally|furthermore|;|,\s+(?:then|also)|और|फिर|तथा|साथ\s+ही)\s+",
    )
    .expect("valid segmenter split regex")
});

/// Attempt to split `text` into multiple command segments.
/// Returns `vec![text]` (unchanged) if no split point is found or if the
/// verb classifier only found one imperative verb.
pub fn segment(text: &str, imperative_verb_count: usize) -> Vec<String> {
    if imperative_verb_count < 2 {
        return vec![text.to_string()];
    }
    let parts: Vec<String> = SPLIT_RE
        .split(text)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if parts.len() >= 2 {
        parts
    } else {
        vec![text.to_string()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_simple_and() {
        let result = segment("Mute the laptop and email my boss I'm offline", 2);
        assert_eq!(result.len(), 2, "expected 2 segments, got: {:?}", result);
    }

    #[test]
    fn no_split_single_verb() {
        let result = segment("Search for the email from John", 1);
        assert_eq!(result, vec!["Search for the email from John"]);
    }

    #[test]
    fn splits_hindi_conjunction() {
        let result = segment("Volume band karo और boss ko email karo", 2);
        assert_eq!(result.len(), 2, "expected 2 segments for Hindi and");
    }
}
