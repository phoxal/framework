//! Speaker capability: subscribes `component::speaker::Chunk` and plays it on
//! the Webots `Speaker` device.
//!
//! Webots cannot be streamed into. `wb_speaker_play_sound` takes "the path to
//! the sound file that should be played" - a WAV file on disk, with no
//! buffer-facing entry point at all. So a stream is accumulated here and played
//! as one sound when the producer ends it with `None`. That is the whole reason
//! the contract has an explicit end-of-stream rather than a bare byte chunk.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use phoxal::api;
use phoxal::model::component::CapabilityRef;

/// The largest stream this controller will hold before the producer ends it.
/// A speaker that is never told the sound finished must not be able to consume
/// the machine; 64 MiB is minutes of uncompressed audio.
const MAX_STREAM_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct SpeakerSpec {
    pub(crate) reference: CapabilityRef,
}

pub(crate) struct NativeSpeaker {
    speaker: webots_rs::device::speaker::Speaker,
    reference: CapabilityRef,
    directory: PathBuf,
    buffer: Vec<u8>,
    playing: Option<PathBuf>,
    sequence: u64,
}

impl NativeSpeaker {
    pub(crate) fn new(webots: &webots_rs::Webots, spec: &SpeakerSpec) -> Result<Self> {
        let speaker = webots
            .speaker(spec.reference.to_string())
            .map_err(|error| anyhow!(error))?;
        let directory = std::env::temp_dir().join(format!("phoxal-speaker-{}", std::process::id()));
        std::fs::create_dir_all(&directory).with_context(|| {
            format!(
                "failed to create the speaker staging directory {}",
                directory.display()
            )
        })?;
        Ok(Self {
            speaker,
            reference: spec.reference.clone(),
            directory,
            buffer: Vec::new(),
            playing: None,
            sequence: 0,
        })
    }

    pub(crate) fn apply(&mut self, chunk: api::component::speaker::Chunk) -> Result<()> {
        match chunk.stream {
            Some(bytes) => {
                if self.buffer.len().saturating_add(bytes.len()) > MAX_STREAM_BYTES {
                    let held = self.buffer.len();
                    self.buffer.clear();
                    bail!(
                        "speaker {} was sent more than {MAX_STREAM_BYTES} bytes ({held} held) \
                         without ending the stream; a stream must be closed with `None`",
                        self.reference
                    );
                }
                self.buffer.extend_from_slice(&bytes);
                Ok(())
            }
            None => self.play_buffered(),
        }
    }

    fn play_buffered(&mut self) -> Result<()> {
        // An empty stream is a producer saying "nothing to play", not an error.
        if self.buffer.is_empty() {
            return Ok(());
        }
        let sound = std::mem::take(&mut self.buffer);
        if !is_wav(&sound) {
            bail!(
                "speaker {} was sent a stream that does not start with a WAV header; the \
                 contract carries WAV-coded audio",
                self.reference
            );
        }

        // Webots keys a sound by its path, so each stream gets its own file:
        // replaying a path whose bytes changed underneath would play the old
        // sound. The previous one is removed once the new one has started.
        self.sequence = self.sequence.saturating_add(1);
        let path = self
            .directory
            .join(format!("{}-{}.wav", self.reference, self.sequence));
        std::fs::write(&path, &sound)
            .with_context(|| format!("failed to stage speaker audio at {}", path.display()))?;

        self.stop()?;
        webots_rs::device::speaker::Speaker::play_sound(
            &self.speaker,
            &self.speaker,
            &path.to_string_lossy(),
            1.0,
            1.0,
            0.0,
            false,
        )
        .map_err(|error| anyhow!(error))?;
        if let Some(previous) = self.playing.replace(path) {
            let _ = std::fs::remove_file(previous);
        }
        Ok(())
    }

    /// Stops whatever this speaker is playing. Used before starting the next
    /// sound and when the controller shuts down, so a stopped simulation does
    /// not leave audio running.
    pub(crate) fn stop(&self) -> Result<()> {
        let Some(playing) = self.playing.as_ref() else {
            return Ok(());
        };
        self.speaker
            .stop(&playing.to_string_lossy())
            .map_err(|error| anyhow!(error))
    }
}

impl Drop for NativeSpeaker {
    fn drop(&mut self) {
        if let Some(playing) = self.playing.take() {
            let _ = std::fs::remove_file(playing);
        }
        remove_if_empty(&self.directory);
    }
}

fn remove_if_empty(directory: &Path) {
    if directory
        .read_dir()
        .is_ok_and(|mut entries| entries.next().is_none())
    {
        let _ = std::fs::remove_dir(directory);
    }
}

/// Whether a stream carries the standard WAV container the contract promises.
fn is_wav(sound: &[u8]) -> bool {
    sound.len() >= 12 && &sound[0..4] == b"RIFF" && &sound[8..12] == b"WAVE"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_riff_wave_header_is_accepted() {
        let mut sound = Vec::from(*b"RIFF");
        sound.extend_from_slice(&36u32.to_le_bytes());
        sound.extend_from_slice(b"WAVE");
        assert!(is_wav(&sound));
    }

    #[test]
    fn anything_else_is_rejected_before_webots_sees_it() {
        assert!(!is_wav(b""));
        assert!(!is_wav(b"RIFF"));
        assert!(!is_wav(b"RIFF____AVI "));
        assert!(!is_wav(b"\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b"));
    }
}
