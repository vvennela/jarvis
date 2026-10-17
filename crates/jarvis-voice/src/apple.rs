//! Bounded Apple SpeechAnalyzer process adapter. PCM stays in memory.
use std::{
    io::{Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const MAX_OUTPUT: usize = 64 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppleSpeechError {
    Configuration,
    InvalidAudio,
    Cancelled,
    Timeout,
    Unavailable,
    InvalidResponse,
    ResponseTooLarge,
}
pub struct AppleTranscriber {
    executable: PathBuf,
    timeout: Duration,
}
impl AppleTranscriber {
    pub fn new(executable: PathBuf, timeout: Duration) -> Result<Self, AppleSpeechError> {
        if !executable.is_absolute() || timeout.is_zero() || timeout > Duration::from_secs(30) {
            return Err(AppleSpeechError::Configuration);
        }
        let metadata =
            std::fs::symlink_metadata(&executable).map_err(|_| AppleSpeechError::Configuration)?;
        if !metadata.is_file() {
            return Err(AppleSpeechError::Configuration);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                return Err(AppleSpeechError::Configuration);
            }
        }
        Ok(Self {
            executable,
            timeout,
        })
    }

    #[cfg(unix)]
    pub fn transcribe(
        &self,
        samples: &[f32],
        cancelled: Arc<AtomicBool>,
    ) -> Result<String, AppleSpeechError> {
        self.transcribe_after(samples, None, cancelled)
    }

    /// With `skip_before` (16 kHz samples), keep only words that start at or
    /// after that point. The result may be empty when only the wake phrase
    /// was heard. Helpers without word timing return the full text.
    #[cfg(unix)]
    pub fn transcribe_after(
        &self,
        samples: &[f32],
        skip_before: Option<usize>,
        cancelled: Arc<AtomicBool>,
    ) -> Result<String, AppleSpeechError> {
        if cancelled.load(Ordering::Acquire) {
            return Err(AppleSpeechError::Cancelled);
        }
        crate::validate_audio(samples).map_err(|_| AppleSpeechError::InvalidAudio)?;
        let wav = wav_bytes(samples);
        let deadline = Instant::now() + self.timeout;
        let mut child = Command::new(&self.executable)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| AppleSpeechError::Unavailable)?;
        let result = (|| {
            let input = child.stdin.take().ok_or(AppleSpeechError::Unavailable)?;
            let mut output = child.stdout.take().ok_or(AppleSpeechError::Unavailable)?;
            for pipe in [
                &input as &dyn std::os::fd::AsFd,
                &output as &dyn std::os::fd::AsFd,
            ] {
                let flags =
                    rustix::fs::fcntl_getfl(pipe).map_err(|_| AppleSpeechError::Unavailable)?;
                rustix::fs::fcntl_setfl(pipe, flags | rustix::fs::OFlags::NONBLOCK)
                    .map_err(|_| AppleSpeechError::Unavailable)?;
            }
            let mut input = Some(input);
            let mut written = 0;
            let mut body = Vec::with_capacity(1024);
            let mut buffer = [0_u8; 4096];
            let mut exited = None;
            loop {
                if cancelled.load(Ordering::Acquire) {
                    return Err(AppleSpeechError::Cancelled);
                }
                if Instant::now() >= deadline {
                    return Err(AppleSpeechError::Timeout);
                }
                if let Some(writer) = input.as_mut() {
                    match writer.write(&wav[written..]) {
                        Ok(0) => return Err(AppleSpeechError::Unavailable),
                        Ok(n) => written += n,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(_) => return Err(AppleSpeechError::Unavailable),
                    }
                }
                if written == wav.len() {
                    drop(input.take());
                }
                let mut eof = false;
                loop {
                    match output.read(&mut buffer) {
                        Ok(0) => {
                            eof = true;
                            break;
                        }
                        Ok(n) => {
                            if n > MAX_OUTPUT - body.len() {
                                return Err(AppleSpeechError::ResponseTooLarge);
                            }
                            body.extend_from_slice(&buffer[..n]);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(_) => return Err(AppleSpeechError::Unavailable),
                    }
                }
                if exited.is_none() {
                    exited = child
                        .try_wait()
                        .map_err(|_| AppleSpeechError::Unavailable)?;
                }
                if let Some(status) = exited {
                    if !status.success() {
                        return Err(AppleSpeechError::Unavailable);
                    }
                    if eof {
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
            let value: serde_json::Value =
                serde_json::from_slice(&body).map_err(|_| AppleSpeechError::InvalidResponse)?;
            let text = value
                .get("text")
                .and_then(serde_json::Value::as_str)
                .ok_or(AppleSpeechError::InvalidResponse)?;
            if text.len() > crate::MAX_TRANSCRIPT_BYTES {
                return Err(AppleSpeechError::ResponseTooLarge);
            }
            if text.trim().is_empty() || text.contains('\0') {
                return Err(AppleSpeechError::InvalidResponse);
            }
            match (
                skip_before,
                value.get("words").and_then(serde_json::Value::as_array),
            ) {
                (Some(skip), Some(words)) if !words.is_empty() => after_skip(words, skip),
                _ => Ok(text.trim().to_owned()),
            }
        })();
        if result.is_err() {
            let _ = child.kill();
        }
        let _ = child.wait();
        result
    }
}

fn after_skip(words: &[serde_json::Value], skip: usize) -> Result<String, AppleSpeechError> {
    if words.len() > 2048 {
        return Err(AppleSpeechError::ResponseTooLarge);
    }
    let mut kept = String::new();
    for word in words {
        let text = word
            .get("text")
            .and_then(serde_json::Value::as_str)
            .ok_or(AppleSpeechError::InvalidResponse)?;
        let start_ms = word
            .get("start_ms")
            .and_then(serde_json::Value::as_u64)
            .ok_or(AppleSpeechError::InvalidResponse)?;
        if text.contains('\0') {
            return Err(AppleSpeechError::InvalidResponse);
        }
        if start_ms.saturating_mul(u64::from(crate::SAMPLE_RATE) / 1000) < skip as u64 {
            continue;
        }
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if !kept.is_empty() {
            kept.push(' ');
        }
        kept.push_str(text);
        if kept.len() > crate::MAX_TRANSCRIPT_BYTES {
            return Err(AppleSpeechError::ResponseTooLarge);
        }
    }
    Ok(kept.trim().to_owned())
}

fn wav_bytes(samples: &[f32]) -> Vec<u8> {
    let size = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + size as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + size).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&crate::SAMPLE_RATE.to_le_bytes());
    bytes.extend_from_slice(&(crate::SAMPLE_RATE * 2).to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&size.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(
            &((sample * 32768.0).round().clamp(-32768.0, 32767.0) as i16).to_le_bytes(),
        );
    }
    bytes
}
