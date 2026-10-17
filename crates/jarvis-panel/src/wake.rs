//! Local speech-energy endpointing and exact wake phrase recognition.
//! Ambient decode text is inspected transiently, never returned as a command.
const RATE: usize = 16_000;
const SILENCE: usize = RATE * 7 / 10;
const WAKE_WINDOW: usize = RATE * 4;
const DECODE_INTERVAL: usize = RATE * 5 / 2;
const COMMAND_CAP: usize = RATE * 15;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Wake,
    Command,
}
pub struct DecodeRequest {
    pub samples: Vec<f32>,
    pub phase: Phase,
    pub endpoint: bool,
}
pub struct WakeDetector {
    phase: Phase,
    audio: Vec<f32>,
    active: bool,
    silence: usize,
    since_decode: usize,
    waiting: usize,
    pending: Option<(Phase, bool)>,
}
impl Default for WakeDetector {
    fn default() -> Self {
        Self::new()
    }
}
impl WakeDetector {
    pub fn new() -> Self {
        Self {
            phase: Phase::Wake,
            audio: Vec::with_capacity(COMMAND_CAP),
            active: false,
            silence: 0,
            since_decode: 0,
            waiting: 0,
            pending: None,
        }
    }
    pub fn phase(&self) -> Phase {
        self.phase
    }
    pub fn reset(&mut self) {
        self.phase = Phase::Wake;
        self.clear_audio();
        self.pending = None;
    }
    fn clear_audio(&mut self) {
        self.audio.clear();
        self.active = false;
        self.silence = 0;
        self.since_decode = 0;
        self.waiting = 0;
    }
    pub fn ingest(&mut self, samples: &[f32]) -> Option<DecodeRequest> {
        if self.pending.is_some() {
            return None;
        }
        if samples.iter().any(|s| !s.is_finite()) {
            self.reset();
            return None;
        }
        for frame in samples.chunks(320) {
            let energy =
                frame.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / frame.len() as f64;
            let voiced = energy >= 0.012_f64.powi(2);
            self.waiting += frame.len();
            if voiced {
                if !self.active {
                    self.since_decode = 0;
                }
                self.active = true;
                self.silence = 0;
            } else {
                self.silence += frame.len();
            }
            let cap = if self.phase == Phase::Wake {
                WAKE_WINDOW
            } else {
                COMMAND_CAP
            };
            if self.phase == Phase::Command && self.audio.len() + frame.len() > cap {
                self.audio
                    .extend_from_slice(&frame[..cap - self.audio.len()]);
                break;
            }
            if self.audio.len() + frame.len() > cap {
                let excess = self.audio.len() + frame.len() - cap;
                self.audio.drain(..excess.min(self.audio.len()));
            }
            self.audio.extend_from_slice(frame);
            if self.phase == Phase::Command && self.audio.len() >= COMMAND_CAP {
                break;
            }
            self.since_decode += frame.len();
            if !self.active && self.audio.len() > RATE / 10 {
                self.audio.drain(..self.audio.len() - RATE / 10);
            }
            if self.phase == Phase::Command && !self.active && self.waiting >= RATE * 10 {
                self.reset();
            }
        }
        let endpoint = self.active
            && (self.silence >= SILENCE
                || (self.phase == Phase::Command && self.audio.len() >= COMMAND_CAP));
        if self.active
            && (endpoint || (self.phase == Phase::Wake && self.since_decode >= DECODE_INTERVAL))
        {
            self.pending = Some((self.phase, endpoint));
            self.since_decode = 0;
            return Some(DecodeRequest {
                samples: self.audio.clone(),
                phase: self.phase,
                endpoint,
            });
        }
        None
    }
    pub fn accept_transcript(&mut self, text: &str) -> Option<String> {
        let (phase, endpoint) = self.pending.take()?;
        if text.len() > 8192 || text.contains('\0') {
            self.reset();
            return None;
        }
        match phase {
            Phase::Wake => {
                if let Some(tail) = wake_tail(text) {
                    self.phase = Phase::Command;
                    if endpoint {
                        self.clear_audio();
                        if !tail.is_empty() {
                            self.reset();
                            return Some(tail.to_owned());
                        }
                    }
                } else if endpoint {
                    self.clear_audio();
                }
                None
            }
            Phase::Command => {
                let command = wake_tail(text).unwrap_or(text).trim();
                let result = (!command.is_empty()).then(|| command.to_owned());
                self.reset();
                result
            }
        }
    }
}
/// Word boundaries prevent accidental matches such as "Jarvises" or "Jarvis123".
pub fn wake_tail(text: &str) -> Option<&str> {
    let words: Vec<_> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect();
    for word in words {
        if word.eq_ignore_ascii_case("jarvis") {
            let end = word.as_ptr() as usize - text.as_ptr() as usize + word.len();
            return Some(
                text[end..]
                    .trim_start_matches(|c: char| c.is_whitespace() || c.is_ascii_punctuation()),
            );
        }
    }
    None
}
