use std::path::Path;

/// Image preprocessing: metadata extraction, thumbnail generation, OCR placeholder.
pub struct ImageProcessor;

#[derive(Debug, Clone)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub size_bytes: u64,
}

impl ImageProcessor {
    /// Extract basic image information.
    pub fn info(path: &Path) -> anyhow::Result<ImageInfo> {
        let meta = std::fs::metadata(path)?;
        let img = image::open(path)?;
        let (w, h) = (img.width(), img.height());
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("unknown")
            .to_lowercase();

        Ok(ImageInfo {
            width: w,
            height: h,
            format: ext,
            size_bytes: meta.len(),
        })
    }

    /// Resize image to fit within max dimension, preserving aspect ratio.
    pub fn thumbnail(path: &Path, max_dim: u32) -> anyhow::Result<Vec<u8>> {
        let img = image::open(path)?;
        let thumb = img.thumbnail(max_dim, max_dim);
        let mut buf = Vec::new();
        thumb.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)?;
        Ok(buf)
    }

    /// Encode image to base64 for LLM vision APIs.
    pub fn to_base64(path: &Path) -> anyhow::Result<String> {
        let data = std::fs::read(path)?;
        Ok(base64_encode(&data))
    }
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}
