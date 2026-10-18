#![cfg(unix)]
//! Multi-turn voice conversations through the production wake worker, voice
//! handoff, conversation worker and tool runtime. Only the microphone, model,
//! Spotify catalog/player and Mail are fixtures; nothing audible or external.
#[path = "../../../tests/support/mod.rs"]
mod support;
use jarvis::{
    context::{ContextStore, Limits},
    conversation::Conversation,
    email::EmailClient,
    music_catalog::{CatalogConfig, MusicCatalog},
    providers::{CrusoeClient, NEMOTRON_MODEL, ProviderConfig},
    spotify_playback::SpotifyPlayback,
    tool_runtime::ToolRuntime,
};
use jarvis_panel::{
    chat::ConversationWorker,
    speech::SpeechWorker,
    voicehandoff::VoiceHandoff,
    wakeworker::{State, WakeBackend, WakeWorker},
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use support::TempDir;

const ARTIST_ID: &str = "0123456789ABCDEFGHIJKL";
const TRACK_ID: &str = "abcdefghijklmnopqrstuv";

/// Blocking loopback HTTP server: every request body is recorded and answered
/// by `respond(path, body)`.
struct Server {
    url: String,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}
impl Server {
    fn new(respond: impl Fn(&str, &Value) -> Value + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let handle = thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                let (mut socket, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => break,
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 8192];
                let (path, body) = loop {
                    let n = socket.read(&mut buffer).unwrap_or(0);
                    if n == 0 {
                        break (String::new(), Value::Null);
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&bytes[..end]).to_string();
                        let path = header.split_whitespace().nth(1).unwrap_or("").to_owned();
                        let length = header
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            let body = serde_json::from_slice(&bytes[end + 4..end + 4 + length])
                                .unwrap_or(Value::Null);
                            break (path, body);
                        }
                    }
                };
                let reply = respond(&path, &body).to_string();
                captured.lock().unwrap().push((path, body));
                let wire = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = socket.write_all(wire.as_bytes());
            }
        });
        Self {
            url,
            requests,
            stop,
            handle: Some(handle),
        }
    }
    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
    fn bodies(&self) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|(_, body)| body.clone())
            .collect()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            handle.join().unwrap();
        }
    }
}

fn text(content: &str) -> Value {
    json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":content}}]})
}
fn tool(name: &str, args: Value) -> Value {
    json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[{"id":"call-1","type":"function","function":{"name":name,"arguments":args.to_string()}}]}}]})
}

#[derive(Default)]
struct Audio {
    chunks: Mutex<VecDeque<Vec<f32>>>,
    texts: Mutex<VecDeque<String>>,
}
impl Audio {
    /// One spoken utterance: a second of speech, then silence to end it.
    fn say(&self, text: &str) {
        self.texts.lock().unwrap().push_back(text.into());
        let mut chunks = self.chunks.lock().unwrap();
        chunks.push_back(vec![0.1; 16000]);
        chunks.push_back(vec![0.0; 17600]);
    }
}
struct Backend(Arc<Audio>);
impl WakeBackend for Backend {
    fn start(&mut self) -> Result<(), &'static str> {
        Ok(())
    }
    fn stop(&mut self) {}
    fn poll(&mut self) -> Result<(Vec<f32>, bool), &'static str> {
        Ok((
            self.0
                .chunks
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default(),
            false,
        ))
    }
    fn transcribe(&mut self, _: &[f32], _: Arc<AtomicBool>) -> Result<String, &'static str> {
        Ok(self.0.texts.lock().unwrap().pop_front().unwrap_or_default())
    }
}

struct Rig {
    dir: TempDir,
    model: Server,
    catalog: Server,
    audio: Arc<Audio>,
    wake: WakeWorker,
    chat: ConversationWorker,
    speech: SpeechWorker,
    handoff: VoiceHandoff,
    spoken_count: usize,
}

impl Rig {
    /// `artist`: the name the fake Spotify catalog and player will verify.
    fn new(model_replies: Vec<Value>, artist: &str) -> Self {
        let dir = TempDir::new();
        let database = dir.join("context.db");
        drop(ContextStore::open(&database, Limits::default()).unwrap());
        let replies = Arc::new(Mutex::new(VecDeque::from(model_replies)));
        let model = Server::new(move |_, _| {
            replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| text("Fixture answer."))
        });
        let name = artist.to_owned();
        let catalog = Server::new(move |path, _| {
            if path.starts_with("/search") {
                json!({"results":{"web":[{"url":format!("https://open.spotify.com/artist/{ARTIST_ID}"),"title":"t","description":"d"}]}})
            } else {
                json!({"provider_name":"Spotify","provider_url":"https://spotify.com","type":"rich","title":name,"html":"<iframe></iframe>"})
            }
        });
        // Fake Spotify player: records its arguments, reports the artist playing.
        let player = dir.join("player");
        fs::write(
            dir.join("observed"),
            json!({"status":"observed","state":"playing","artist":artist,"track_id":format!("spotify:track:{TRACK_ID}"),"track_url":format!("https://open.spotify.com/track/{TRACK_ID}")}).to_string(),
        )
        .unwrap();
        fs::write(
            &player,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"${0%/*}/played\"\n/bin/cat \"${0%/*}/observed\"\n",
        )
        .unwrap();
        fs::set_permissions(&player, fs::Permissions::from_mode(0o700)).unwrap();
        // Fake osascript for Mail/Contacts: logs sends, knows one contact.
        let mail = dir.join("osascript");
        fs::write(
            &mail,
            "#!/bin/sh\nshift\ncase \"$1\" in\n  *Contacts*) printf 'ok\\nBrian Chen\\tbrian@example.com\\n';;\n  *Mail*) shift; printf '%s|' \"$@\" >> \"${0%/*}/sent\"; printf 'sent';;\nesac\n",
        )
        .unwrap();
        fs::set_permissions(&mail, fs::Permissions::from_mode(0o700)).unwrap();
        let model_url = model.url.clone();
        let catalog_url = catalog.url.clone();
        let dir_path = dir.path().to_path_buf();
        let chat = ConversationWorker::with_factory(move || {
            let lm = CrusoeClient::new(
                "fixture-only",
                NEMOTRON_MODEL,
                ProviderConfig::loopback_for_tests(&model_url, Duration::from_secs(3)).unwrap(),
            )
            .unwrap();
            let runtime = ToolRuntime::read_only(database.clone())
                .with_music(
                    MusicCatalog::new(
                        "fixture-key",
                        CatalogConfig::loopback_for_tests(
                            &format!("{catalog_url}/search"),
                            &format!("{catalog_url}/oembed"),
                            Duration::from_secs(3),
                        )
                        .unwrap(),
                    )
                    .unwrap(),
                    SpotifyPlayback::with_executable(
                        dir_path.join("player"),
                        Duration::from_secs(3),
                    )
                    .unwrap(),
                )
                .with_email(
                    EmailClient::with_executable(
                        dir_path.join("osascript"),
                        Duration::from_secs(3),
                    )
                    .unwrap(),
                )
                .with_preferences(jarvis::preferences::PreferenceStore::new(
                    dir_path.join("preferences.json"),
                ));
            let mut conversation = Conversation::new(database.clone(), None, lm);
            conversation.set_tool_runtime(Arc::new(runtime));
            Ok(conversation)
        })
        .unwrap();
        let speaker = dir.join("speaker");
        fs::write(
            &speaker,
            "#!/bin/sh\ncat >> \"${0%/*}/spoken\"\nprintf '\\n--said--\\n' >> \"${0%/*}/spoken\"\nsleep 0.05\n",
        )
        .unwrap();
        fs::set_permissions(&speaker, fs::Permissions::from_mode(0o700)).unwrap();
        let speech = SpeechWorker::with_executable(speaker).unwrap();
        let audio = Arc::new(Audio::default());
        let backend = audio.clone();
        let wake = WakeWorker::with_backend(move || Ok(Box::new(Backend(backend)))).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !(chat.control.available() && speech.control.available()) {
            assert!(Instant::now() < deadline, "workers did not start");
            thread::sleep(Duration::from_millis(5));
        }
        wake.control.set_enabled(true);
        let mut rig = Self {
            dir,
            model,
            catalog,
            audio,
            wake,
            chat,
            speech,
            handoff: VoiceHandoff::conversational(),
            spoken_count: 0,
        };
        rig.until("wake listening", |r| {
            r.wake.control.snapshot().state == State::WakeListening
        });
        rig
    }
    fn tick(&mut self) {
        self.handoff
            .tick(&self.wake.control, &self.chat.control, &self.speech.control);
    }
    fn until(&mut self, what: &str, mut done: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            self.tick();
            if done(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; spoken so far: {:?}",
                self.spoken()
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn pump(&mut self, duration: Duration) {
        let end = Instant::now() + duration;
        while Instant::now() < end {
            self.tick();
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn spoken(&self) -> Vec<String> {
        fs::read_to_string(self.dir.join("spoken"))
            .unwrap_or_default()
            .split("\n--said--\n")
            .filter(|part| !part.trim().is_empty())
            .map(str::to_owned)
            .collect()
    }
    /// Say something and return Jarvis's spoken reply, once playback ends.
    fn exchange(&mut self, utterance: &str) -> String {
        self.audio.say(utterance);
        let before = self.spoken_count;
        self.until(&format!("reply to {utterance:?}"), |r| {
            r.spoken().len() > before && !r.speech.control.busy() && !r.chat.control.busy()
        });
        let spoken = self.spoken();
        self.spoken_count = spoken.len();
        // Let the follow-up or conversational window open after playback.
        self.pump(Duration::from_millis(250));
        spoken.last().unwrap().clone()
    }
    /// Say something that must produce no reply at all.
    fn ignored(&mut self, utterance: &str) {
        self.audio.say(utterance);
        self.pump(Duration::from_millis(700));
        assert_eq!(
            self.spoken().len(),
            self.spoken_count,
            "{utterance:?} produced a reply: {:?}",
            self.spoken().last()
        );
    }
    fn listening_without_wake(&self) -> bool {
        self.wake.control.snapshot().state == State::CommandListening
    }
    fn played(&self) -> String {
        fs::read_to_string(self.dir.join("played")).unwrap_or_default()
    }
    fn instructions_sent_with(&self, request_index: usize) -> String {
        self.model.bodies()[request_index]["messages"][0]["content"]
            .as_str()
            .unwrap_or("")
            .to_owned()
    }
    fn saved_preferences(&self) -> Vec<String> {
        jarvis::preferences::PreferenceStore::new(self.dir.join("preferences.json"))
            .list()
            .into_iter()
            .map(|preference| preference.text)
            .collect()
    }
    fn new_wake(&mut self) {
        self.wake.control.cancel_followup();
        self.pump(Duration::from_millis(100));
    }
    fn history_sent_with(&self, request_index: usize) -> Vec<Value> {
        let body = &self.model.bodies()[request_index];
        let packet = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|message| {
                message["content"]
                    .as_str()?
                    .strip_prefix("Context packet (history and source evidence):\n")
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        serde_json::from_str::<Value>(&packet)
            .ok()
            .and_then(|packet| packet["history"].as_array().cloned())
            .unwrap_or_default()
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
fn open_ended_music_request_asks_then_plays_the_answer_without_the_model() {
    for (opener, answer, artist) in [
        ("play some music", "Kanye", "Kanye"),
        ("put on some music", "Kanye please", "Kanye"),
        (
            "I want to listen to music",
            "how about Kanye West",
            "Kanye West",
        ),
        ("play something", "um, Kanye West", "Kanye West"),
        ("throw on some tunes", "Daft Punk", "Daft Punk"),
    ] {
        let mut rig = Rig::new(vec![], artist);
        let question = rig.exchange(&format!("Jarvis {opener}"));
        assert_eq!(question, "Sure, who do you want to hear?", "{opener}");
        assert!(rig.listening_without_wake(), "{opener}: no answer window");
        let reply = rig.exchange(answer);
        assert!(
            rig.played().contains(artist),
            "{opener} / {answer}: nothing played; reply {reply:?}"
        );
        assert!(reply.contains(artist), "{reply}");
        assert_eq!(rig.model.count(), 0, "{opener}: model was consulted");
        assert_eq!(rig.catalog.count(), 2);
    }
}

#[test]
fn filler_while_jarvis_waits_for_an_answer_is_not_an_answer() {
    let mut rig = Rig::new(vec![], "Kanye");
    rig.exchange("Jarvis play some music");
    rig.ignored("um");
    assert!(rig.played().is_empty(), "filler was played as an artist");
    assert_eq!(rig.catalog.count(), 0);
}

#[test]
fn a_new_wake_does_not_answer_an_old_question() {
    let mut rig = Rig::new(vec![text("Kanye's a rapper and producer.")], "Kanye");
    rig.exchange("Jarvis play some music");
    // The window is left to the next explicit wake instead of an answer.
    rig.wake.control.cancel_followup();
    rig.pump(Duration::from_millis(100));
    let reply = rig.exchange("Jarvis Kanye");
    assert!(
        rig.played().is_empty(),
        "stale question was answered: {reply}"
    );
    assert_eq!(rig.model.count(), 1);
    assert!(
        rig.history_sent_with(0).is_empty(),
        "fresh wake carried history"
    );
}

#[test]
fn hands_free_follow_up_keeps_the_conversation() {
    let mut rig = Rig::new(
        vec![
            text("Finish the Beacon launch checklist."),
            text("Because the launch is Friday."),
        ],
        "Kanye",
    );
    let first = rig.exchange("Jarvis what should I work on");
    assert_eq!(first, "Finish the Beacon launch checklist.");
    assert!(rig.listening_without_wake());
    let second = rig.exchange("and why is that the priority");
    assert_eq!(second, "Because the launch is Friday.");
    let history = rig.history_sent_with(1);
    assert_eq!(history.len(), 1, "{history:?}");
    assert_eq!(
        history[0]["assistant"],
        "Finish the Beacon launch checklist."
    );
}

#[test]
fn a_fresh_hey_jarvis_forgets_the_previous_conversation() {
    let mut rig = Rig::new(
        vec![
            text("Finish the Beacon checklist."),
            text("Because the launch is Friday."),
            text("I'm not sure what you mean."),
        ],
        "Kanye",
    );
    rig.exchange("Jarvis what should I work on");
    rig.exchange("and why is that");
    assert_eq!(rig.history_sent_with(1).len(), 1);
    rig.wake.control.cancel_followup();
    rig.pump(Duration::from_millis(100));
    rig.exchange("Jarvis what did we just talk about");
    assert_eq!(rig.model.count(), 3);
    assert!(
        rig.history_sent_with(2).is_empty(),
        "new session carried {:?}",
        rig.history_sent_with(2)
    );
}

#[test]
fn after_music_plays_the_conversation_continues_hands_free_with_context() {
    let mut rig = Rig::new(vec![text("Yeah, Kanye's playing now.")], "Kanye");
    rig.exchange("Jarvis play some music");
    let playing = rig.exchange("Kanye");
    assert!(rig.played().contains("Kanye"), "{playing}");
    assert!(
        rig.listening_without_wake(),
        "no window after playback reply"
    );
    let reply = rig.exchange("is that the right artist");
    assert_eq!(reply, "Yeah, Kanye's playing now.");
    let history = rig.history_sent_with(0);
    assert!(
        history
            .iter()
            .any(|turn| turn["user"].as_str().unwrap_or("").contains("Kanye")),
        "{history:?}"
    );
}

#[test]
fn email_is_read_back_and_sent_by_voice_without_repeating_the_wake_word() {
    let mut rig = Rig::new(
        vec![tool(
            "compose_email",
            json!({"to":"Brian","subject":"Running late","body":"I'll be ten minutes late."}),
        )],
        "Kanye",
    );
    let read_back = rig.exchange("Jarvis email Brian that I'm running ten minutes late");
    assert!(read_back.contains("Brian Chen"), "{read_back}");
    assert!(read_back.contains("ten minutes late"), "{read_back}");
    assert!(!rig.dir.join("sent").exists(), "composing sent mail");
    assert!(rig.listening_without_wake());
    let sent = rig.exchange("send it");
    assert_eq!(sent, "Sent it to Brian Chen.");
    assert_eq!(
        fs::read_to_string(rig.dir.join("sent")).unwrap(),
        "brian@example.com|Running late|I'll be ten minutes late.|"
    );
    assert_eq!(rig.model.count(), 1, "sending consulted the model");
}

#[test]
fn email_can_be_read_back_again_and_a_bare_yes_then_sends() {
    let mut rig = Rig::new(
        vec![tool(
            "compose_email",
            json!({"to":"Brian","subject":"Lunch","body":"Lunch at noon?"}),
        )],
        "Kanye",
    );
    rig.exchange("Jarvis email Brian about lunch at noon");
    let again = rig.exchange("read it back");
    assert!(again.contains("Lunch at noon?"), "{again}");
    assert!(!rig.dir.join("sent").exists());
    rig.exchange("yes");
    assert!(
        fs::read_to_string(rig.dir.join("sent"))
            .unwrap()
            .contains("Lunch at noon?")
    );
}

#[test]
fn saying_no_to_the_read_back_never_sends() {
    let mut rig = Rig::new(
        vec![tool(
            "compose_email",
            json!({"to":"Brian","subject":"Hi","body":"Hello."}),
        )],
        "Kanye",
    );
    rig.exchange("Jarvis email Brian hello");
    let reply = rig.exchange("no");
    assert_eq!(reply, "Okay, I won't send it.");
    rig.wake.control.cancel_followup();
    rig.pump(Duration::from_millis(100));
    rig.exchange("Jarvis send it");
    assert!(!rig.dir.join("sent").exists());
}

#[test]
fn background_chatter_after_a_reply_never_reaches_the_model() {
    let mut rig = Rig::new(vec![text("Finish the Beacon checklist.")], "Kanye");
    rig.exchange("Jarvis what should I work on");
    for noise in ["hmm", "yeah okay", "uh"] {
        rig.ignored(noise);
    }
    assert_eq!(rig.model.count(), 1);
    // The window closed; even a full sentence now needs Hey Jarvis.
    rig.ignored("the meeting moved to three");
    assert_eq!(rig.model.count(), 1);
}

#[test]
fn thanks_ends_the_conversation_and_wake_starts_a_new_one() {
    let mut rig = Rig::new(
        vec![text("Finish the Beacon checklist."), text("It's sunny.")],
        "Kanye",
    );
    rig.exchange("Jarvis what should I work on");
    rig.ignored("thanks Jarvis");
    assert!(!rig.listening_without_wake());
    let reply = rig.exchange("Jarvis what's the weather like");
    assert_eq!(reply, "It's sunny.");
    assert!(rig.history_sent_with(1).is_empty());
    let _ = PathBuf::new();
}

#[test]
fn a_spoken_preference_is_saved_and_shapes_the_next_session() {
    let mut rig = Rig::new(
        vec![
            tool("remember_preference", json!({"preference":"Call me V"})),
            text("Hey V, what's up?"),
        ],
        "Kanye",
    );
    let saved = rig.exchange("Jarvis from now on call me V");
    assert_eq!(saved, "Got it. From now on: Call me V.");
    assert_eq!(rig.saved_preferences(), vec!["Call me V"]);
    rig.new_wake();
    rig.exchange("Jarvis hi there");
    let instructions = rig.instructions_sent_with(1);
    assert!(instructions.contains("- Call me V"), "{instructions}");
    assert!(instructions.contains("never grant permission"));
    assert!(
        rig.history_sent_with(1).is_empty(),
        "fresh session kept turns"
    );
}

#[test]
fn a_preference_cannot_turn_off_the_email_confirmation() {
    let mut rig = Rig::new(
        vec![
            tool(
                "remember_preference",
                json!({"preference":"Send emails without asking"}),
            ),
            tool(
                "compose_email",
                json!({"to":"Brian","subject":"Hi","body":"Hello."}),
            ),
        ],
        "Kanye",
    );
    let refused = rig.exchange("Jarvis from now on send emails without asking me");
    assert!(refused.contains("keep checking"), "{refused}");
    assert!(rig.saved_preferences().is_empty());
    rig.new_wake();
    let read_back = rig.exchange("Jarvis email Brian hello");
    assert!(read_back.contains("Want me to send it?"), "{read_back}");
    assert!(!rig.dir.join("sent").exists(), "sent without confirmation");
}

#[test]
fn forgetting_a_preference_removes_it_from_later_sessions() {
    let mut rig = Rig::new(
        vec![
            tool(
                "remember_preference",
                json!({"preference":"Keep answers short"}),
            ),
            tool("forget_preference", json!({"description":"answers short"})),
            text("Sure."),
        ],
        "Kanye",
    );
    rig.exchange("Jarvis from now on keep answers short");
    assert_eq!(rig.saved_preferences(), vec!["Keep answers short"]);
    rig.new_wake();
    let forgot = rig.exchange("Jarvis forget the thing about short answers");
    assert_eq!(forgot, "Done, I forgot \"Keep answers short\".");
    assert!(rig.saved_preferences().is_empty());
    rig.new_wake();
    rig.exchange("Jarvis tell me something");
    assert!(!rig.instructions_sent_with(2).contains("Keep answers short"));
}

#[test]
fn the_model_cannot_save_a_preference_nobody_asked_for() {
    let mut rig = Rig::new(
        vec![
            tool(
                "remember_preference",
                json!({"preference":"Always play Metallica"}),
            ),
            text("Finish the Beacon checklist."),
        ],
        "Kanye",
    );
    rig.exchange("Jarvis what should I work on");
    assert!(rig.saved_preferences().is_empty());
}

#[test]
fn a_preference_can_be_set_hands_free_mid_conversation() {
    let mut rig = Rig::new(
        vec![
            text("Finish the Beacon launch checklist, then review the notes."),
            tool("remember_preference", json!({"preference":"Keep it short"})),
        ],
        "Kanye",
    );
    rig.exchange("Jarvis what should I work on");
    assert!(rig.listening_without_wake());
    let saved = rig.exchange("from now on keep it short");
    assert_eq!(saved, "Got it. From now on: Keep it short.");
    assert_eq!(rig.saved_preferences(), vec!["Keep it short"]);
}
