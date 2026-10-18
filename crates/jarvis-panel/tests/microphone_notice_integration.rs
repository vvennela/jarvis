#![cfg(unix)]
//! Actual handoff/workers plus a silent owned playback fixture; no microphone,
//! provider, OS routing change, or audible speech.
#[path = "../../../tests/support/mod.rs"]
mod support;
use jarvis::conversation::ConversationError;
use jarvis_panel::{
    chat::ConversationWorker,
    speech::SpeechWorker,
    voicehandoff::VoiceHandoff,
    wakeworker::{State, WakeBackend, WakeWorker},
};
use jarvis_voice::capture::InputInfo;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use support::TempDir;
#[derive(Default)]
struct Input {
    info: Mutex<Option<InputInfo>>,
    changed: AtomicBool,
    fallback: AtomicBool,
}
impl Input {
    fn set(&self, name: Option<&str>) {
        *self.info.lock().unwrap() = name.map(|name| InputInfo {
            name: name.into(),
            sample_rate: 24000,
            channels: 1,
        });
        self.changed.store(true, Ordering::Release);
    }
}
struct Backend(Arc<Input>);
impl WakeBackend for Backend {
    fn start(&mut self) -> Result<(), &'static str> {
        Ok(())
    }
    fn stop(&mut self) {}
    fn listening_message(&self) -> Option<&'static str> {
        self.0
            .fallback
            .load(Ordering::Acquire)
            .then_some("AirPods microphone stopped responding. Using the MacBook microphone.")
    }
    fn input_info(&self) -> Option<InputInfo> {
        self.0.info.lock().unwrap().clone()
    }
    fn poll(&mut self) -> Result<(Vec<f32>, bool), &'static str> {
        Ok((Vec::new(), self.0.changed.swap(false, Ordering::AcqRel)))
    }
    fn transcribe(&mut self, _: &[f32], _: Arc<AtomicBool>) -> Result<String, &'static str> {
        panic!("notice fixture must not decode speech")
    }
}
fn wait(mut f: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(4);
    while !f() {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(3));
    }
}
struct Rig {
    dir: TempDir,
    input: Arc<Input>,
    wake: WakeWorker,
    chat: ConversationWorker,
    speech: SpeechWorker,
    handoff: VoiceHandoff,
}
impl Rig {
    fn new() -> Self {
        let dir = TempDir::new();
        assert!(dir.path().is_dir());
        let exe = dir.join("speaker");
        fs::write(&exe, "#!/bin/sh\ncat >> \"${0%/*}/spoken\"\nprintf '\\n--utterance--\\n' >> \"${0%/*}/spoken\"\nwhile [ -f \"${0%/*}/hold\" ]; do sleep 0.01; done\n").unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
        let speech = SpeechWorker::with_executable(exe).unwrap();
        let chat = ConversationWorker::with_factory(|| {
            Err(ConversationError::MissingKey("CRUSOE_API_KEY"))
        })
        .unwrap();
        let input = Arc::new(Input::default());
        input.set(Some("Private Owner AirPods"));
        let copied = input.clone();
        let wake = WakeWorker::with_backend(move || Ok(Box::new(Backend(copied)))).unwrap();
        wait(|| speech.control.available() && chat.is_finished());
        wake.control.set_enabled(true);
        wait(|| wake.control.snapshot().state == State::WakeListening);
        let mut rig = Self {
            dir,
            input,
            wake,
            chat,
            speech,
            handoff: VoiceHandoff::new(),
        };
        rig.tick();
        rig
    }
    fn tick(&mut self) {
        self.handoff
            .tick(&self.wake.control, &self.chat.control, &self.speech.control);
    }
    fn until(&mut self, mut f: impl FnMut(&Self) -> bool) {
        let end = Instant::now() + Duration::from_secs(4);
        loop {
            self.tick();
            if f(self) {
                return;
            }
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(3));
        }
    }
    fn pump(&mut self, duration: Duration) {
        let end = Instant::now() + duration;
        while Instant::now() < end {
            self.tick();
            thread::sleep(Duration::from_millis(3));
        }
    }
    fn spoken(&self) -> String {
        fs::read_to_string(self.dir.join("spoken")).unwrap_or_default()
    }
}
impl Drop for Rig {
    fn drop(&mut self) {
        self.wake.control.shutdown();
        self.chat.control.stop();
        self.speech.control.shutdown();
        self.wake.join().unwrap();
        self.chat.join().unwrap();
        self.speech.join().unwrap();
    }
}

#[test]
fn initial_input_is_silent_and_changed_microphone_is_shown_once_but_never_spoken() {
    let mut r = Rig::new();
    r.pump(Duration::from_millis(50));
    assert!(r.spoken().is_empty());
    assert!(!r.handoff.take_microphone_reveal());
    r.input.set(None);
    wait(|| r.wake.control.snapshot().diagnostics.input.is_none());
    r.tick();
    r.input.fallback.store(true, Ordering::Release);
    r.input.set(Some("MacBook Pro Microphone"));
    r.until(|r| r.handoff.microphone_notice().is_some());
    let notice = r.handoff.microphone_notice().unwrap().to_owned();
    assert_eq!(
        notice,
        "AirPods microphone stopped responding. Using the MacBook microphone."
    );
    assert!(!notice.contains("Private Owner"));
    assert!(r.handoff.take_microphone_reveal());
    assert!(!r.handoff.take_microphone_reveal());
    r.pump(Duration::from_millis(150));
    assert!(
        r.spoken().is_empty(),
        "microphone change was announced aloud"
    );
}
#[test]
fn muted_microphone_changes_wait_until_listening_resumes() {
    let mut r = Rig::new();
    r.wake.control.set_enabled(false);
    wait(|| r.wake.control.snapshot().state == State::Muted);
    r.input.set(Some("MacBook Pro Microphone"));
    r.pump(Duration::from_millis(100));
    assert!(r.handoff.microphone_notice().is_none());
    assert!(!r.handoff.take_microphone_reveal());
    r.wake.control.set_enabled(true);
    r.until(|r| r.handoff.microphone_notice().is_some());
    assert!(r.handoff.take_microphone_reveal());
    r.pump(Duration::from_millis(100));
    assert!(r.spoken().is_empty());
}
#[test]
fn owned_speech_is_not_interrupted_and_latest_selected_microphone_notice_waits() {
    let mut r = Rig::new();
    fs::write(r.dir.join("hold"), "").unwrap();
    r.speech.control.speak("Existing response.".into()).unwrap();
    r.until(|r| r.spoken().contains("Existing response."));
    r.input.set(Some("Intermediate USB input"));
    r.pump(Duration::from_millis(30));
    r.input.set(Some("MacBook Pro Microphone"));
    r.pump(Duration::from_millis(40));
    assert!(r.speech.control.busy());
    assert!(r.handoff.microphone_notice().is_none());
    fs::remove_file(r.dir.join("hold")).unwrap();
    r.until(|r| r.handoff.microphone_notice().is_some() && !r.speech.control.busy());
    let notice = r.handoff.microphone_notice().unwrap();
    assert!(notice.contains("MacBook"), "{notice}");
    assert!(!notice.contains("Intermediate USB"));
    assert_eq!(r.spoken().matches("--utterance--").count(), 1);
}

#[test]
fn fallback_hold_tracks_the_physical_microphone_across_bluetooth_profile_changes() {
    use jarvis_panel::wakeworker::same_input_device;
    use jarvis_voice::capture::input_route_token_with_format;
    let headset = input_route_token_with_format("fixture:headset", 24000, 1, "F32").unwrap();
    let profile_changed =
        input_route_token_with_format("fixture:headset", 48000, 2, "I16").unwrap();
    let different = input_route_token_with_format("fixture:builtin", 48000, 2, "F32").unwrap();
    assert!(same_input_device(&headset, &headset));
    assert!(same_input_device(&headset, &profile_changed));
    assert!(!same_input_device(&headset, &different));
    assert!(!same_input_device("invalid", "invalid"));
    assert!(!same_input_device(&headset, "invalid"));
}
