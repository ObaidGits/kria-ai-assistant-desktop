//! Streaming sentence splitter for piping LLM token output into TTS.
//!
//! The biggest TTFA win in v2 is *not* waiting for the full agent response
//! before starting synthesis. Instead, we feed LLM tokens through a
//! [`SentenceSplitter`], emit each completed sentence to TTS as soon as it
//! lands, and start playback while the rest of the response is still being
//! generated.
//!
//! Splitting on `.`, `?`, `!`, `…`, `।` (Devanagari danda — for Hinglish), but
//! suppressing splits inside common abbreviations (`Mr.`, `Dr.`, `e.g.`, …)
//! and after single-letter initials (`A.`, `B.`).

use std::collections::HashSet;

use once_cell::sync::Lazy;

/// Punctuation that ends a sentence. Keep ASCII + the Devanagari danda for
/// Hinglish utterances that may slip through the post-edit.
const SENTENCE_TERMINATORS: &[char] = &['.', '!', '?', '…', '।'];

/// Common abbreviations whose trailing dot must NOT trigger a split.
static ABBREVIATIONS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        "mr", "mrs", "ms", "dr", "prof", "sr", "jr", "st", "vs", "etc", "e.g", "i.e", "approx",
        "no", "vol", "fig", "ft", "ph", "u.s", "u.k", "a.m", "p.m",
    ]
    .into_iter()
    .collect()
});

/// Minimum characters in a "sentence" before we are willing to flush it.
/// Prevents firing TTS on every "Yes." and "OK." in a streaming reply.
const MIN_SENTENCE_LEN: usize = 4;

/// Push LLM tokens in via [`SentenceSplitter::push`]; pull completed
/// sentences out as `Vec<String>`. Call [`SentenceSplitter::flush`] at end of
/// stream to emit any tail-buffer that wasn't terminated.
#[derive(Debug, Default)]
pub struct SentenceSplitter {
    buf: String,
}

impl SentenceSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a token (or arbitrary chunk) and return any newly-complete
    /// sentences, in order.
    pub fn push(&mut self, token: &str) -> Vec<String> {
        self.buf.push_str(token);
        self.drain_complete()
    }

    /// Force-emit anything still buffered. Call at end of stream.
    pub fn flush(&mut self) -> Option<String> {
        let trimmed = self.buf.trim();
        if trimmed.is_empty() {
            self.buf.clear();
            return None;
        }
        let out = trimmed.to_string();
        self.buf.clear();
        Some(out)
    }

    fn drain_complete(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        loop {
            let Some(end) = next_sentence_end(&self.buf) else {
                break;
            };
            // `end` is the byte index AFTER the terminator (exclusive).
            let sentence: String = self.buf[..end].trim().to_string();
            if sentence.len() >= MIN_SENTENCE_LEN {
                out.push(sentence);
                self.buf.drain(..end);
            } else {
                // Too short — wait for more.
                break;
            }
        }
        out
    }
}

/// Find the first valid sentence boundary in `s`. Returns `None` if no
/// sentence ends in the buffer yet.
fn next_sentence_end(s: &str) -> Option<usize> {
    let mut iter = s.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        if !SENTENCE_TERMINATORS.contains(&c) {
            continue;
        }
        // Greedy-consume contiguous terminators (`?!`, `...`).
        let mut end_byte = i + c.len_utf8();
        while let Some(&(_, nc)) = iter.peek() {
            if SENTENCE_TERMINATORS.contains(&nc) {
                end_byte += nc.len_utf8();
                iter.next();
            } else {
                break;
            }
        }
        // Look at the next char (if any) — must be a boundary.
        if let Some(&(_, next_c)) = iter.peek() {
            if !is_boundary(next_c) {
                continue;
            }
        }
        // Suppress abbreviations: look back at the preceding word.
        if c == '.' && is_abbreviation(&s[..i]) {
            continue;
        }
        return Some(end_byte);
    }
    None
}

fn is_boundary(c: char) -> bool {
    c.is_whitespace() || c == '"' || c == '\'' || c == ')' || c == ']'
}

fn is_abbreviation(prefix: &str) -> bool {
    let last_word: String = prefix
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '.')
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    if last_word.len() == 1 {
        // Single capital letter followed by `.` — initial. Don't split.
        return last_word
            .chars()
            .next()
            .map(|c| c.is_ascii_alphabetic())
            .unwrap_or(false);
    }
    let lower = last_word.to_ascii_lowercase();
    ABBREVIATIONS.contains(lower.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_period() {
        let mut s = SentenceSplitter::new();
        let out = s.push("Hello world. This is fine.");
        assert_eq!(out, vec!["Hello world.", "This is fine."]);
    }

    #[test]
    fn streams_token_by_token() {
        let mut s = SentenceSplitter::new();
        let mut out = Vec::new();
        for tok in ["Hel", "lo", " wor", "ld.", " Next", " one", "?"] {
            out.extend(s.push(tok));
        }
        out.extend(s.flush());
        assert_eq!(out, vec!["Hello world.", "Next one?"]);
    }

    #[test]
    fn suppresses_abbreviation() {
        let mut s = SentenceSplitter::new();
        let out = s.push("Dr. Smith spoke. Then he left.");
        assert_eq!(out, vec!["Dr. Smith spoke.", "Then he left."]);
    }

    #[test]
    fn suppresses_initial() {
        let mut s = SentenceSplitter::new();
        let out = s.push("J. R. R. Tolkien wrote books. The end.");
        assert_eq!(out, vec!["J. R. R. Tolkien wrote books.", "The end."]);
    }

    #[test]
    fn handles_devanagari_danda() {
        let mut s = SentenceSplitter::new();
        let out = s.push("Mujhe meeting schedule karni hai। Kal subah।");
        assert_eq!(
            out,
            vec![
                "Mujhe meeting schedule karni hai।",
                "Kal subah।"
            ]
        );
    }

    #[test]
    fn flush_returns_tail() {
        let mut s = SentenceSplitter::new();
        let _ = s.push("incomplete sentence with no");
        assert_eq!(s.flush(), Some("incomplete sentence with no".to_string()));
        assert_eq!(s.flush(), None);
    }

    #[test]
    fn min_length_guard() {
        let mut s = SentenceSplitter::new();
        // "Hi." is below MIN_SENTENCE_LEN (4) — should buffer.
        let out = s.push("Hi.");
        assert!(out.is_empty(), "very short sentences should buffer");
    }
}
