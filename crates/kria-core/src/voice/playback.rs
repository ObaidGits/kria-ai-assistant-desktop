use rodio::{Sink, OutputStream};

/// Audio player using rodio for TTS output playback.
pub struct AudioPlayer;

impl AudioPlayer {
    pub fn new() -> Self {
        Self
    }

    /// Play WAV file.
    pub async fn play_file(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let path = path.to_path_buf();
        // rodio is sync, so run in blocking thread
        tokio::task::spawn_blocking(move || {
            let (_stream, handle) = OutputStream::try_default()?;
            let sink = Sink::try_new(&handle)?;
            let file = std::io::BufReader::new(std::fs::File::open(&path)?);
            let source = rodio::Decoder::new(file)?;
            sink.append(source);
            sink.sleep_until_end();
            Ok::<_, anyhow::Error>(())
        }).await??;
        Ok(())
    }

    /// Play raw PCM f32 samples.
    pub async fn play_samples(&self, samples: Vec<f32>, sample_rate: u32) -> anyhow::Result<()> {
        tokio::task::spawn_blocking(move || {
            let (_stream, handle) = OutputStream::try_default()?;
            let sink = Sink::try_new(&handle)?;
            let source = rodio::buffer::SamplesBuffer::new(1, sample_rate, samples);
            sink.append(source);
            sink.sleep_until_end();
            Ok::<_, anyhow::Error>(())
        }).await??;
        Ok(())
    }
}
