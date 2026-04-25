use std::path::PathBuf;

/// Strip markdown formatting and other non-speech characters from `text` before
/// handing it to Piper/espeak-ng.
///
/// espeak-ng phonemises raw `*`, `_`, `` ` ``, `#` etc. literally ("asterisk",
/// "hash", …). This function removes those markup artefacts so the synthesised
/// speech sounds natural.
pub fn normalize_for_tts(text: &str) -> String {
    use regex::Regex;

    // 1. Remove triple-backtick code fences (spoken as "backtick backtick backtick")
    //    Use a non-greedy match with DOTALL via (?s).
    let text = Regex::new(r"(?s)```.*?```").map_or_else(
        |_| text.to_string(),
        |re| re.replace_all(text, "").into_owned(),
    );

    // 2. Remove inline code (`code`)
    let text = Regex::new(r"`[^`\n]*`").map_or_else(
        |_| text.clone(),
        |re| re.replace_all(&text, "").into_owned(),
    );

    // 3. Strip markdown bold/italic (**text**, *text*), unwrapping the inner text
    let text = Regex::new(r"\*{1,3}([^*\n]+)\*{1,3}").map_or_else(
        |_| text.clone(),
        |re| re.replace_all(&text, "$1").into_owned(),
    );
    // 4. Strip markdown underline/italic (__text__, _text_), unwrapping inner text
    let text = Regex::new(r"_{1,2}([^_\n]+)_{1,2}").map_or_else(
        |_| text.clone(),
        |re| re.replace_all(&text, "$1").into_owned(),
    );

    // 5. Strip markdown headers (## Heading → Heading)
    let text = Regex::new(r"(?m)^#{1,6}\s+").map_or_else(
        |_| text.clone(),
        |re| re.replace_all(&text, "").into_owned(),
    );

    // 6. Strip bullet list markers (- / * / + at line start, numbered lists)
    let text = Regex::new(r"(?m)^[-*+]\s+").map_or_else(
        |_| text.clone(),
        |re| re.replace_all(&text, "").into_owned(),
    );
    let text = Regex::new(r"(?m)^\d+\.\s+").map_or_else(
        |_| text.clone(),
        |re| re.replace_all(&text, "").into_owned(),
    );

    // 7. Normalise ellipsis (… unicode or ... → natural comma-pause)
    let text = text.replace('…', ", ");
    let text = Regex::new(r"\.{2,}").map_or_else(
        |_| text.clone(),
        |re| re.replace_all(&text, ", ").into_owned(),
    );

    // 8. Replace em-dash / en-dash with a natural pause
    let text = text.replace('—', ", ").replace('–', ", ");

    // 9. Strip leftover bare special chars that espeak would vocalise literally
    let text = Regex::new(r"[*_#~|\\]").map_or_else(
        |_| text.clone(),
        |re| re.replace_all(&text, "").into_owned(),
    );

    // 10. Replace URLs with a spoken placeholder
    let text = Regex::new(r"https?://\S+").map_or_else(
        |_| text.clone(),
        |re| re.replace_all(&text, "the link").into_owned(),
    );

    // 11. Collapse newlines and multiple spaces into a single space
    let text = Regex::new(r"[\r\n]+").map_or_else(
        |_| text.clone(),
        |re| re.replace_all(&text, " ").into_owned(),
    );
    let text = Regex::new(r" {2,}").map_or_else(
        |_| text.clone(),
        |re| re.replace_all(&text, " ").into_owned(),
    );

    text.trim().to_string()
}

/// Text-to-Speech using Piper (ONNX voice models).
///
/// Production: use `ort` crate for in-process ONNX inference.
/// Current: shells out to piper binary.
pub struct TextToSpeech {
    model_path: PathBuf,
    config_path: PathBuf,
    /// Piper binary path (if using CLI mode).
    binary_path: Option<PathBuf>,
    sample_rate: u32,
}

impl TextToSpeech {
    pub fn new(model_path: PathBuf, binary_path: Option<PathBuf>) -> Self {
        let config_path = model_path.with_extension("onnx.json");
        Self {
            model_path,
            config_path,
            binary_path,
            sample_rate: 22050,
        }
    }

    /// Synthesize speech from text, returning WAV file path.
    ///
    /// Text is normalised before synthesis: markdown formatting, code fences,
    /// stray punctuation characters and URLs are stripped so that espeak-ng
    /// does not literally speak symbols like "asterisk" or "hash".
    pub async fn synthesize(&self, text: &str) -> anyhow::Result<PathBuf> {
        let clean = normalize_for_tts(text);
        let output_path = std::env::temp_dir().join("kria_tts_output.wav");

        if let Some(ref binary) = self.binary_path {
            let mut child = tokio::process::Command::new(binary)
                .args([
                    "--model",
                    &self.model_path.to_string_lossy(),
                    "--config",
                    &self.config_path.to_string_lossy(),
                    "--output_file",
                    &output_path.to_string_lossy(),
                    // Slightly faster tempo (0.95×) sounds more natural than the
                    // piper default (1.0×) for conversational responses.
                    "--length-scale", "0.95",
                    // Increased generator noise adds natural pitch micro-variation.
                    "--noise-scale", "0.8",
                    // Reduced phoneme-duration noise keeps rhythm stable while
                    // still avoiding the robotic fixed-cadence feel.
                    "--noise-w", "0.6",
                ])
                .stdin(std::process::Stdio::piped())
                .spawn()?;

            if let Some(mut stdin) = child.stdin.take() {
                use tokio::io::AsyncWriteExt;
                stdin.write_all(clean.as_bytes()).await?;
            }

            let status = child.wait().await?;
            if !status.success() {
                anyhow::bail!("piper TTS failed");
            }

            Ok(output_path)
        } else {
            anyhow::bail!("piper-rs bindings not yet implemented; provide binary_path")
        }
    }

    /// Synthesize and return raw PCM samples (f32).
    pub async fn synthesize_samples(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let wav_path = self.synthesize(text).await?;
        let data = std::fs::read(&wav_path)?;
        let _ = std::fs::remove_file(&wav_path);

        // Skip WAV header (44 bytes) and convert i16 to f32
        if data.len() < 44 {
            anyhow::bail!("invalid WAV file");
        }

        let samples: Vec<f32> = data[44..]
            .chunks_exact(2)
            .map(|chunk| {
                let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
                sample as f32 / 32768.0
            })
            .collect();

        Ok(samples)
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_for_tts;

    #[test]
    fn strips_markdown_bold_and_italic() {
        assert_eq!(normalize_for_tts("I am **very** happy"), "I am very happy");
        assert_eq!(normalize_for_tts("I am *really* sure"), "I am really sure");
    }

    #[test]
    fn strips_markdown_headers() {
        assert_eq!(normalize_for_tts("## Summary\nHello"), "Summary Hello");
    }

    #[test]
    fn strips_inline_code() {
        let out = normalize_for_tts("Use `cargo build` to compile");
        assert!(!out.contains('`'), "backticks should be removed");
        assert!(out.contains("to compile"), "surrounding text should remain");
    }

    #[test]
    fn strips_code_fences() {
        let input = "Here:\n```rust\nlet x = 1;\n```\nDone.";
        let out = normalize_for_tts(input);
        assert!(!out.contains("```"), "code fence should be removed");
        assert!(out.contains("Done."), "text after fence should remain");
    }

    #[test]
    fn replaces_ellipsis_with_pause() {
        assert_eq!(normalize_for_tts("Wait...okay"), "Wait, okay");
        assert_eq!(normalize_for_tts("Wait\u{2026}okay"), "Wait, okay");
    }

    #[test]
    fn replaces_url_with_placeholder() {
        let out = normalize_for_tts("See https://example.com for details");
        assert!(!out.contains("https://"), "URL should be replaced");
        assert!(out.contains("the link"), "should contain placeholder");
    }

    #[test]
    fn plain_text_passes_through_unchanged() {
        let text = "Hello, how are you today?";
        assert_eq!(normalize_for_tts(text), text);
    }

    #[test]
    fn collapses_newlines() {
        assert_eq!(normalize_for_tts("line one\nline two"), "line one line two");
    }
}
