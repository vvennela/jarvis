//! Local speech-energy endpointing and exact wake phrase recognition.
//! Ambient decode text is inspected transiently, never returned as a command.
const RATE: usize = 16_000;
const SILENCE: usize = RATE * 11 / 10;
const START_RMS: f64 = 0.012;
const CONTINUE_RMS: f64 = 0.006;
const WAKE_WINDOW: usize = RATE * 4;
const FIRST_DECODE_INTERVAL: usize = RATE * 6 / 5;
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
pub struct DecodeOutcome {
    pub command: Option<String>,
    pub next: Option<DecodeRequest>,
}
pub struct WakeDetector {
    phase: Phase,
    audio: Vec<f32>,
    active: bool,
    silence: usize,
    since_decode: usize,
    probed: bool,
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
            probed: false,
            waiting: 0,
            pending: None,
        }
    }
    pub fn phase(&self) -> Phase {
        self.phase
    }
    /// A dedicated acoustic spotter has already heard the invocation. Preserve
    /// its bounded keyword/tail audio so Whisper can transcribe a same-breath request.
    pub fn arm_keyword(&mut self, audio: &[f32]) {
        self.reset();
        self.phase = Phase::Command;
        self.active = true;
        let start = audio.len().saturating_sub(WAKE_WINDOW);
        self.audio.extend_from_slice(&audio[start..]);
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
        self.probed = false;
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
            // Once speech starts, quieter trailing words still keep the turn
            // open. An explicit wake also arms quiet followup speech; idle
            // background sound must still cross the higher start gate.
            let voiced = voiced_frame(frame, self.active || self.phase == Phase::Command);
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
        // Probe a new utterance promptly, but retain the slower repeat cadence
        // for ongoing ambient speech so local inference stays bounded.
        let interval = if self.probed {
            DECODE_INTERVAL
        } else {
            FIRST_DECODE_INTERVAL
        };
        if self.active && (endpoint || (self.phase == Phase::Wake && self.since_decode >= interval))
        {
            self.pending = Some((self.phase, endpoint));
            self.probed = true;
            self.since_decode = 0;
            return Some(DecodeRequest {
                samples: self.audio.clone(),
                phase: self.phase,
                endpoint,
            });
        }
        None
    }
    /// Audio keeps arriving while local inference runs. An endpoint is only
    /// final if that backlog contains no resumed speech. Retain both sides of
    /// a hesitation and decode them together instead of submitting the prefix.
    pub fn accept_transcript_with_audio(&mut self, text: &str, queued: &[f32]) -> DecodeOutcome {
        let empty = || DecodeOutcome {
            command: None,
            next: None,
        };
        let Some((phase, endpoint)) = self.pending else {
            return empty();
        };
        if text.len() > 8192
            || text.contains('\0')
            || queued.len() > COMMAND_CAP
            || queued.iter().any(|sample| !sample.is_finite())
        {
            self.reset();
            return empty();
        }
        let resumed = queued.chunks(320).any(|frame| voiced_frame(frame, true));
        if endpoint && resumed && (phase == Phase::Wake || self.audio.len() < COMMAND_CAP) {
            self.pending = None;
            if phase == Phase::Command || wake_tail(text).is_some() {
                self.phase = Phase::Command;
            }
            return DecodeOutcome {
                command: None,
                next: self.ingest(queued),
            };
        }
        let command = self.accept_transcript(text);
        let next = if command.is_none() {
            self.ingest(queued)
        } else {
            None
        };
        DecodeOutcome { command, next }
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
                // Only strip invocation prefixes here. A substantive command
                // such as "tell me about Jarvis" must remain intact.
                let invocation = leading_wake_tail(text).map(strip_repeated_wakes);
                let command = invocation.unwrap_or(text).trim();
                if invocation.is_some() && command.is_empty() {
                    self.clear_audio();
                    return None;
                }
                let result = (!command.is_empty()).then(|| command.to_owned());
                self.reset();
                result
            }
        }
    }
}
fn voiced_frame(frame: &[f32], active: bool) -> bool {
    if frame.is_empty() {
        return false;
    }
    let energy = frame.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / frame.len() as f64;
    let threshold = if active { CONTINUE_RMS } else { START_RMS };
    energy >= threshold * threshold
}
/// Word boundaries prevent accidental matches such as "Jarvises" or "Jarvis123".
pub fn wake_tail(text: &str) -> Option<&str> {
    let words = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty());
    for word in words {
        if word.eq_ignore_ascii_case("jarvis") {
            let end = word.as_ptr() as usize - text.as_ptr() as usize + word.len();
            return Some(strip_repeated_wakes(trim_invocation_separator(
                &text[end..],
            )));
        }
    }
    None
}

fn trim_invocation_separator(text: &str) -> &str {
    text.trim_start_matches(|c: char| c.is_whitespace() || c.is_ascii_punctuation())
}

fn leading_wake_tail(text: &str) -> Option<&str> {
    let mut words = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty());
    let mut word = words.next()?;
    if word.eq_ignore_ascii_case("hey") {
        word = words.next()?;
    }
    if !word.eq_ignore_ascii_case("jarvis") {
        return None;
    }
    let end = word.as_ptr() as usize - text.as_ptr() as usize + word.len();
    Some(trim_invocation_separator(&text[end..]))
}

fn strip_repeated_wakes(mut tail: &str) -> &str {
    while let Some(rest) = leading_wake_tail(tail) {
        tail = rest;
    }
    tail
}
