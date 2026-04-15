/// Token budget management for context window optimization.
pub struct TokenBudget {
    max_tokens: usize,
    reserved_system: usize,
    reserved_response: usize,
}

impl TokenBudget {
    pub fn new(max_tokens: usize) -> Self {
        Self {
            max_tokens,
            reserved_system: 500,
            reserved_response: 1000,
        }
    }

    /// Available tokens for user context (messages + tool results).
    pub fn available(&self) -> usize {
        self.max_tokens
            .saturating_sub(self.reserved_system)
            .saturating_sub(self.reserved_response)
    }

    /// Estimate token count for text (rough: chars / 4).
    pub fn estimate_tokens(text: &str) -> usize {
        // Rough approximation; replace with tiktoken-rs for accuracy
        text.len() / 4
    }

    /// Truncate text to fit within a token budget.
    pub fn truncate(text: &str, max_tokens: usize) -> String {
        let max_chars = max_tokens * 4;
        if text.len() <= max_chars {
            text.to_string()
        } else {
            let truncated = &text[..max_chars.min(text.len())];
            // Find last word boundary
            if let Some(pos) = truncated.rfind(char::is_whitespace) {
                format!("{}... [truncated]", &truncated[..pos])
            } else {
                format!("{}... [truncated]", truncated)
            }
        }
    }

    /// Allocate budget across multiple content pieces proportionally.
    pub fn allocate(&self, pieces: &[&str]) -> Vec<usize> {
        let total_est: usize = pieces.iter().map(|p| Self::estimate_tokens(p)).sum();
        let available = self.available();

        if total_est <= available {
            return pieces.iter().map(|p| Self::estimate_tokens(p)).collect();
        }

        // Proportional allocation
        pieces.iter()
            .map(|p| {
                let est = Self::estimate_tokens(p);
                (est as f64 / total_est as f64 * available as f64) as usize
            })
            .collect()
    }
}
