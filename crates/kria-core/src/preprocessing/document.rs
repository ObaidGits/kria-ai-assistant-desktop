use std::path::Path;

/// Document preprocessing: extract text from common file formats.
pub struct DocumentProcessor;

impl DocumentProcessor {
    /// Extract text from a file based on extension.
    pub async fn extract_text(path: &Path) -> anyhow::Result<String> {
        let ext = path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        match ext.as_str() {
            "txt" | "md" | "log" | "csv" | "json" | "toml" | "yaml" | "yml" => {
                Ok(tokio::fs::read_to_string(path).await?)
            }
            "pdf" => Self::extract_pdf(path).await,
            "docx" => Self::extract_docx(path).await,
            "html" | "htm" => Self::extract_html(path).await,
            _ => anyhow::bail!("unsupported document format: {ext}"),
        }
    }

    async fn extract_pdf(path: &Path) -> anyhow::Result<String> {
        let p = path.to_path_buf();
        let output = tokio::process::Command::new("pdftotext")
            .args(&[p.to_string_lossy().to_string(), "-".to_string()])
            .output()
            .await?;

        if !output.status.success() {
            anyhow::bail!("pdftotext failed");
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    async fn extract_docx(path: &Path) -> anyhow::Result<String> {
        let p = path.to_path_buf();
        let output = tokio::process::Command::new("pandoc")
            .args(["-t", "plain", &p.to_string_lossy()])
            .output()
            .await?;

        if !output.status.success() {
            anyhow::bail!("pandoc failed for docx extraction");
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    async fn extract_html(path: &Path) -> anyhow::Result<String> {
        let html = tokio::fs::read_to_string(path).await?;
        let document = scraper::Html::parse_document(&html);
        let text: String = document.root_element()
            .text()
            .collect::<Vec<_>>()
            .join(" ");
        Ok(text.split_whitespace().collect::<Vec<_>>().join(" "))
    }
}
