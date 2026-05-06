use crate::audio::Recording;
use crate::models::WhisperModel;
use crate::settings::Settings;
use std::path::{Path, PathBuf};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

#[derive(Default)]
pub struct TranscriptionService {
    active_model: Option<WhisperModel>,
    context: Option<WhisperContext>,
}

impl TranscriptionService {
    pub fn transcribe(
        &mut self,
        recording: &Recording,
        settings: &Settings,
        model_path: &Path,
    ) -> Result<String, String> {
        if recording.pcm_i16.is_empty() {
            return Err("No audio samples were captured".to_string());
        }

        if !model_path.is_file() {
            return Err(format!(
                "Whisper model is missing: {}",
                model_path.display()
            ));
        }

        self.ensure_context(settings.model, model_path)?;

        let context = self
            .context
            .as_ref()
            .ok_or_else(|| "Whisper context is unavailable".to_string())?;
        let mut state = context
            .create_state()
            .map_err(|err| format!("Failed to create Whisper state: {err}"))?;
        let audio = resample_i16_to_16khz_f32(&recording.pcm_i16, recording.sample_rate);
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 0 });
        params.set_n_threads(default_thread_count());
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_no_context(true);

        if settings.language == "auto" {
            params.set_language(None);
            params.set_detect_language(true);
        } else {
            params.set_language(Some(&settings.language));
        }

        state
            .full(params, &audio)
            .map_err(|err| format!("Whisper transcription failed: {err}"))?;

        let transcript = state
            .as_iter()
            .map(|segment| segment.to_string())
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string();

        if transcript.is_empty() {
            return Err("No speech was transcribed".to_string());
        }

        Ok(transcript)
    }

    fn ensure_context(&mut self, model: WhisperModel, model_path: &Path) -> Result<(), String> {
        if self.active_model == Some(model) && self.context.is_some() {
            return Ok(());
        }

        let params = WhisperContextParameters::default();
        let path = path_to_string(model_path)?;
        let context = WhisperContext::new_with_params(&path, params)
            .map_err(|err| format!("Failed to load Whisper model: {err}"))?;

        self.context = Some(context);
        self.active_model = Some(model);
        Ok(())
    }
}

fn resample_i16_to_16khz_f32(samples: &[i16], source_rate: u32) -> Vec<f32> {
    if source_rate == 16_000 {
        return samples
            .iter()
            .map(|sample| f32::from(*sample) / 32768.0)
            .collect();
    }

    let output_len = (samples.len() as u64 * 16_000 / u64::from(source_rate.max(1))) as usize;
    let mut output = Vec::with_capacity(output_len);
    let ratio = source_rate as f64 / 16_000.0;

    for index in 0..output_len {
        let source_pos = index as f64 * ratio;
        let lower = source_pos.floor() as usize;
        let upper = (lower + 1).min(samples.len().saturating_sub(1));
        let weight = (source_pos - lower as f64) as f32;
        let a = f32::from(samples[lower]) / 32768.0;
        let b = f32::from(samples[upper]) / 32768.0;
        output.push(a + (b - a) * weight);
    }

    output
}

fn default_thread_count() -> i32 {
    std::thread::available_parallelism()
        .map(|threads| threads.get().min(4) as i32)
        .unwrap_or(1)
}

fn path_to_string(path: &Path) -> Result<String, String> {
    let absolute: PathBuf = path
        .canonicalize()
        .map_err(|err| format!("Failed to resolve model path: {err}"))?;
    absolute
        .to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| "Model path is not valid UTF-8".to_string())
}

#[cfg(test)]
mod tests {
    use super::resample_i16_to_16khz_f32;

    #[test]
    fn converts_16khz_i16_to_f32_without_resampling() {
        let audio = resample_i16_to_16khz_f32(&[0, 16_384, -16_384], 16_000);

        assert_eq!(audio.len(), 3);
        assert_eq!(audio[0], 0.0);
        assert!((audio[1] - 0.5).abs() < 0.001);
        assert!((audio[2] + 0.5).abs() < 0.001);
    }

    #[test]
    fn downsamples_to_16khz_length() {
        let input = vec![1_000_i16; 48_000];
        let audio = resample_i16_to_16khz_f32(&input, 48_000);

        assert_eq!(audio.len(), 16_000);
    }

    #[test]
    fn handles_empty_input() {
        let audio = resample_i16_to_16khz_f32(&[], 48_000);

        assert!(audio.is_empty());
    }
}
