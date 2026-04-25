//! Media resolver middleware.
//!
//! Downloads remote media attachments to a local cache directory before the
//! message is handed to the agent pipeline.  The resolved [`MediaRef::local_path`]
//! is populated in-place.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tracing::{debug, error, info, warn};

use super::{InboundMessage, MediaRef};

// ── MediaDownloader trait ─────────────────────────────────────────────────────

/// Platform-specific download helper.
///
/// Each platform adapter that deals with media should provide an implementation
/// so the resolver can download files that require authentication (e.g. Telegram
/// file_id expansion).
#[async_trait]
pub trait MediaDownloader: Send + Sync + 'static {
    /// Platform this downloader handles.
    fn platform_id(&self) -> &'static str;

    /// Resolve a [`MediaRef`] to a direct download URL (or download directly
    /// and return the local path).
    ///
    /// Return `None` if the ref cannot be resolved.
    async fn resolve_url(&self, media: &MediaRef) -> Option<String>;
}

// ── MediaResolver ─────────────────────────────────────────────────────────────

/// Downloads all unresolved [`MediaRef`]s in an [`InboundMessage`] to the
/// local cache and updates `local_path` in-place.
pub struct MediaResolver {
    cache_dir: PathBuf,
    downloaders: Arc<Vec<Box<dyn MediaDownloader>>>,
    http: reqwest::Client,
    download_timeout: Duration,
}

impl MediaResolver {
    /// Create a resolver that stores downloads under `cache_dir`.
    pub fn new(cache_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        let cache_dir = cache_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&cache_dir)?;

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()?;

        Ok(Self {
            cache_dir,
            downloaders: Arc::new(vec![]),
            http,
            download_timeout: Duration::from_secs(120),
        })
    }

    /// Register a platform-specific [`MediaDownloader`].
    pub fn register_downloader(&mut self, downloader: Box<dyn MediaDownloader>) {
        Arc::get_mut(&mut self.downloaders)
            .expect("register_downloader called after resolve")
            .push(downloader);
    }

    /// Resolve all media in `msg`, downloading remote refs to the local cache.
    ///
    /// Already-local refs (`local_path.is_some()`) are skipped.
    pub async fn resolve(&self, msg: &mut InboundMessage) {
        for media in msg.media.iter_mut() {
            if media.local_path.is_some() {
                continue; // already resolved
            }
            self.resolve_one(media, &msg.conversation.platform.to_string())
                .await;
        }
    }

    async fn resolve_one(&self, media: &mut MediaRef, platform: &str) {
        // 1. Try platform-specific downloader to get a URL.
        let url = if let Some(dl) = self
            .downloaders
            .iter()
            .find(|d| d.platform_id() == platform)
        {
            dl.resolve_url(media).await
        } else {
            None
        };

        // 2. Fall back to the embedded remote_url.
        let url = url.or_else(|| media.remote_url.clone());

        let Some(url) = url else {
            warn!(
                remote_id = ?media.remote_id,
                "media_resolver: no download URL available for media ref"
            );
            return;
        };

        // 3. Derive a stable filename from the URL.
        let filename = url_to_filename(&url);
        let dest = self.cache_dir.join(&filename);

        // 4. Skip if already cached.
        if dest.exists() {
            debug!(?dest, "media_resolver: cache hit");
            media.local_path = Some(dest);
            return;
        }

        // 5. Download.
        info!(?dest, "media_resolver: downloading");
        match self.download(&url, &dest).await {
            Ok(()) => {
                media.local_path = Some(dest);
            }
            Err(e) => {
                error!("media_resolver: download failed for {url}: {e}");
            }
        }
    }

    async fn download(&self, url: &str, dest: &Path) -> anyhow::Result<()> {
        let response = tokio::time::timeout(
            self.download_timeout,
            self.http.get(url).send(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("download timeout"))?
        .map_err(|e| anyhow::anyhow!("http error: {e}"))?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!("http {}", response.status()));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| anyhow::anyhow!("body error: {e}"))?;

        let mut file = tokio::fs::File::create(dest)
            .await
            .map_err(|e| anyhow::anyhow!("create file: {e}"))?;

        file.write_all(&bytes)
            .await
            .map_err(|e| anyhow::anyhow!("write file: {e}"))?;

        Ok(())
    }
}

/// Convert a URL to a short, filesystem-safe filename using its last path
/// segment and a truncated hash.
fn url_to_filename(url: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    let hash = hasher.finish();

    // Take the last path segment (strip query string).
    let segment = url
        .split('?')
        .next()
        .unwrap_or(url)
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("media");

    // Truncate segment to 32 chars to stay filesystem-safe.
    let segment: String = segment.chars().take(32).collect();
    format!("{segment}_{hash:016x}")
}
