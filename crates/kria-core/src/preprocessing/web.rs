/// Web content preprocessing: fetch and clean HTML pages.
pub struct WebProcessor;

impl WebProcessor {
    /// Fetch a URL and extract clean text content.
    pub async fn fetch_text(url: &str) -> anyhow::Result<String> {
        let resp = reqwest::get(url).await?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("HTTP {status} fetching {url}");
        }
        let html = resp.text().await?;
        Ok(Self::html_to_text(&html))
    }

    /// Extract text from raw HTML.
    pub fn html_to_text(html: &str) -> String {
        let doc = scraper::Html::parse_document(html);

        // Remove script and style elements by skipping them
        let text: String = doc.root_element()
            .text()
            .collect::<Vec<_>>()
            .join(" ");

        // Normalize whitespace
        text.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Extract all links from HTML.
    pub fn extract_links(html: &str) -> Vec<(String, String)> {
        let doc = scraper::Html::parse_document(html);
        let selector = scraper::Selector::parse("a[href]").unwrap();
        doc.select(&selector)
            .filter_map(|el| {
                let href = el.value().attr("href")?.to_string();
                let text = el.text().collect::<String>().trim().to_string();
                Some((href, text))
            })
            .collect()
    }
}
