#![cfg(unix)]
//! Actual worker composition; synthetic local HTTP and silent owned processes only.
#[path = "../../../tests/support/mod.rs"]
mod support;
use jarvis::{
    actions::ActionExecutor,
    app_tools::{AppExecutor, AppPreferences},
    context::{ContextStore, Limits, Source},
    conversation::{Conversation, ConversationError},
    draft::DraftTask,
    providers::{CrusoeClient, NEMOTRON_MODEL, ProviderConfig},
    tool_runtime::ToolRuntime,
};
use jarvis_panel::{
    chat::ConversationWorker,
    tasks::{State as TaskState, TaskWorker},
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use support::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
struct Peer {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    server: tokio::task::JoinHandle<()>,
}
impl Peer {
    async fn new(plans: Vec<Value>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let mut plans = VecDeque::from(plans);
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let n = socket.read(&mut buffer).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    assert!(bytes.len() < 150000);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let size = headers
                            .lines()
                            .find_map(|line| {
                                let (k, v) = line.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + size {
                            captured.lock().unwrap().push(if size == 0 {
                                Value::Null
                            } else {
                                serde_json::from_slice(&bytes[end + 4..end + 4 + size]).unwrap()
                            });
                            break;
                        }
                    }
                }
                let body = plans
                    .pop_front()
                    .unwrap_or_else(|| answer("unexpected extra provider call"))
                    .to_string();
                let _=socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
            }
        });
        Self {
            url,
            requests,
            server,
        }
    }
    fn client(&self) -> CrusoeClient {
        client(&self.url)
    }
    fn packets(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.server.abort();
    }
}
fn client(url: &str) -> CrusoeClient {
    CrusoeClient::new(
        "fixture-only-key",
        NEMOTRON_MODEL,
        ProviderConfig::loopback_for_tests(url, Duration::from_secs(3)).unwrap(),
    )
    .unwrap()
}
fn answer(text: &str) -> Value {
    json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":text}}]})
}
async fn wait(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
fn script(dir: &TempDir, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}
fn seed(dir: &TempDir) {
    assert!(dir.path().is_dir());
    let mut db = ContextStore::open(&dir.join("context.db"), Limits::default()).unwrap();
    db.observe(
        "file:beacon",
        "Current task: Beacon launch handoff for Brian by Friday",
        Source::File,
        1,
    )
    .unwrap();
}
#[tokio::test]
async fn model_tool_call_enqueues_real_draft_once_and_chat_continues_before_review() {
    let dir = TempDir::new();
    seed(&dir);
    let model=Peer::new(vec![json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[{"id":"draft1","type":"function","function":{"name":"delegate_draft","arguments":"{}"}}]}}]}),answer("The Beacon handoff is underway.")]).await;
    let draft_model = Peer::new(vec![answer(
        "Beacon handoff: verify launch checklist before Friday.",
    )])
    .await;
    let endpoint = draft_model.url.clone();
    let opener = script(&dir, "opener", "printf 'opened' > \"${0%/*}/opened\"");
    let mut drafts = TaskWorker::with_factory(
        dir.join("context.db"),
        dir.join("reviews"),
        move |db, job, request| DraftTask::new(db, client(&endpoint), job, request),
        opener,
    )
    .unwrap();
    let runtime = Arc::new(
        ToolRuntime::read_only(dir.join("context.db")).with_draft_submitter(drafts.control.clone()),
    );
    let mut conversation = Conversation::new(dir.join("context.db"), None, model.client());
    conversation.set_tool_runtime(runtime);
    let mut chat = ConversationWorker::with_factory(move || Ok(conversation)).unwrap();
    wait(|| chat.control.available()).await;
    let request = "Draft a Beacon handoff for Brian, ready for my review";
    chat.control.submit(request.into()).unwrap();
    wait(|| !chat.control.busy()).await;
    let acknowledgment = chat.control.snapshot().text;
    assert!(acknowledgment.contains("review"));
    assert!(
        !acknowledgment.contains("I finished") && !acknowledgment.contains("is ready"),
        "queue acknowledgment claimed a completed artifact"
    );
    assert_eq!(model.packets().len(), 1);
    chat.control
        .submit("What did we just start?".into())
        .unwrap();
    wait(|| !chat.control.busy()).await;
    assert_eq!(
        chat.control.snapshot().text,
        "The Beacon handoff is underway."
    );
    wait(|| !drafts.control.busy()).await;
    let ready = drafts.control.snapshot();
    assert_eq!(ready.state, TaskState::ReadyForReview);
    let artifact = fs::read_to_string(ready.path.unwrap()).unwrap();
    assert!(artifact.contains("NOT SENT"));
    assert!(artifact.contains("Beacon handoff"));
    assert!(!dir.join("opened").exists());
    assert_eq!(draft_model.packets().len(), 1);
    let packet: Value = serde_json::from_str(
        draft_model.packets()[0]["messages"][1]["content"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(packet["user_instruction"], request);
    assert_eq!(model.packets().len(), 2);
    chat.control.stop();
    drafts.control.shutdown();
    wait(|| chat.is_finished() && drafts.is_finished()).await;
    chat.join().unwrap();
    drafts.join().unwrap();
}
#[tokio::test]
async fn direct_approved_app_works_without_model_keys_but_quoted_or_negated_text_does_not() {
    let dir = TempDir::new();
    seed(&dir);
    let executable = script(
        &dir,
        "app-adapter",
        "printf '%s\\n' \"$3\" >> \"${0%/*}/effects\"\nprintf 'verified:running\\n'",
    );
    let runtime = Arc::new(
        ToolRuntime::read_only(dir.join("context.db")).with_apps(
            AppExecutor::with_executable(
                AppPreferences::default(),
                executable.clone(),
                Duration::from_secs(1),
            )
            .unwrap(),
        ),
    );
    let action = ActionExecutor::with_executable(executable, Duration::from_secs(1)).unwrap();
    let mut chat = ConversationWorker::with_factory_and_tools(
        || Err(ConversationError::MissingKey("CRUSOE_API_KEY")),
        action,
        runtime,
    )
    .unwrap();
    wait(|| chat.control.available()).await;
    for request in [
        "'open Notes'",
        "Do not open Notes",
        "The document says open Notes",
    ] {
        chat.control.submit(request.into()).unwrap();
        wait(|| !chat.control.busy()).await;
        assert!(!dir.join("effects").exists());
    }
    for app in ["Notes", "Calculator"] {
        chat.control.submit(format!("Open {app}")).unwrap();
        wait(|| !chat.control.busy()).await;
        assert_eq!(chat.control.snapshot().text, format!("{app} is open."));
    }
    assert_eq!(
        fs::read_to_string(dir.join("effects")).unwrap(),
        "com.apple.Notes\ncom.apple.calculator\n"
    );
    chat.control.stop();
    wait(|| chat.is_finished()).await;
    chat.join().unwrap();
}

#[tokio::test]
async fn named_artist_uses_catalog_and_verified_playback_without_model_or_legacy_clarification() {
    use jarvis::{
        music_catalog::{CatalogConfig, MusicCatalog},
        spotify_playback::SpotifyPlayback,
    };
    let dir = TempDir::new();
    seed(&dir);
    let name = "Aurora Fixture Quartet";
    let artist_id = "0123456789ABCDEFGHIJKL";
    let track_id = "abcdefghijklmnopqrstuv";
    let model=Peer::new(vec![json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[{"id":"artist1","type":"function","function":{"name":"spotify_play_artist","arguments":json!({"artist":name}).to_string()}}]}}]})]).await;
    let discovery=Peer::new(vec![json!({"results":{"web":[{"url":format!("https://open.spotify.com/artist/{artist_id}")}]}}),json!({"provider_name":"Spotify","provider_url":"https://spotify.com","type":"rich","title":name})]).await;
    let catalog = MusicCatalog::new(
        "fixture-search-key",
        CatalogConfig::loopback_for_tests(
            &format!("{}/search", discovery.url),
            &format!("{}/oembed", discovery.url),
            Duration::from_secs(2),
        )
        .unwrap(),
    )
    .unwrap();
    let observed = json!({"status":"observed","state":"playing","artist":name,"track_id":format!("spotify:track:{track_id}"),"track_url":format!("https://open.spotify.com/track/{track_id}")});
    fs::write(dir.join("artist-observation"), observed.to_string()).unwrap();
    let playback=SpotifyPlayback::with_executable(script(&dir,"artist-playback","printf '%s\\n' \"$@\" > \"${0%/*}/artist-args\"\n/bin/cat \"${0%/*}/artist-observation\""),Duration::from_secs(1)).unwrap();
    let runtime =
        Arc::new(ToolRuntime::read_only(dir.join("context.db")).with_music(catalog, playback));
    let legacy = ActionExecutor::with_executable(
        script(
            &dir,
            "legacy-playback",
            "printf 'wrong fallback' > \"${0%/*}/legacy-called\"\nprintf 'verified:playing\\n'",
        ),
        Duration::from_secs(1),
    )
    .unwrap();
    let conversation = Conversation::new(dir.join("context.db"), None, model.client());
    let mut chat =
        ConversationWorker::with_factory_and_tools(move || Ok(conversation), legacy, runtime)
            .unwrap();
    wait(|| chat.control.available()).await;
    chat.control
        .submit(format!("Play {name} on Spotify"))
        .unwrap();
    wait(|| !chat.control.busy()).await;
    assert_eq!(chat.control.snapshot().text, format!("Playing {name}."));
    assert!(!dir.join("legacy-called").exists());
    assert_eq!(
        model.packets().len(),
        0,
        "explicit artist intent must not require a model call"
    );
    assert_eq!(discovery.packets().len(), 2);
    let args = fs::read_to_string(dir.join("artist-args")).unwrap();
    assert!(args.ends_with(&format!(
        "spotify:artist:{artist_id}\n{name}\nspotify:artist:{artist_id}\n"
    )));
    chat.control.stop();
    wait(|| chat.is_finished()).await;
    chat.join().unwrap();
}

struct SilentWake;
impl jarvis_panel::wakeworker::WakeBackend for SilentWake {
    fn start(&mut self) -> Result<(), &'static str> {
        Ok(())
    }
    fn stop(&mut self) {}
    fn poll(&mut self) -> Result<(Vec<f32>, bool), &'static str> {
        Ok((vec![], false))
    }
    fn transcribe(
        &mut self,
        _: &[f32],
        _: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<String, &'static str> {
        panic!("silent fixture must never transcribe")
    }
}

#[tokio::test]
async fn model_delegated_completion_speaks_once_restored_ready_is_quiet_and_result_aliases_verify()
{
    use jarvis_panel::{
        speech::SpeechWorker,
        voicehandoff::{Dispatch, VoiceHandoff, dispatch_question},
        wakeworker::WakeWorker,
    };
    let dir = TempDir::new();
    seed(&dir);
    let model = Peer::new(vec![json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[{"id":"draft_notice","type":"function","function":{"name":"delegate_draft","arguments":"{}"}}]}}]})]).await;
    let draft_model = Peer::new(vec![answer("Beacon is ready for review.")]).await;
    let endpoint = draft_model.url.clone();
    let opener = script(
        &dir,
        "notice-opener",
        "printf '%s\\n' \"$1\" >> \"${0%/*}/opened\"",
    );
    let mut task = TaskWorker::with_factory(
        dir.join("context.db"),
        dir.join("reviews"),
        move |db, job, request| DraftTask::new(db, client(&endpoint), job, request),
        opener.clone(),
    )
    .unwrap();
    let runtime =
        ToolRuntime::read_only(dir.join("context.db")).with_draft_submitter(task.control.clone());
    let mut conversation = Conversation::new(dir.join("context.db"), None, model.client());
    conversation.set_tool_runtime(Arc::new(runtime));
    let mut chat = ConversationWorker::with_factory(move || Ok(conversation)).unwrap();
    let mut speech = SpeechWorker::with_executable(script(
        &dir,
        "notice-speaker",
        "cat >> \"${0%/*}/spoken\"\nprintf '\\nEND\\n' >> \"${0%/*}/spoken\"",
    ))
    .unwrap();
    let mut wake = WakeWorker::with_backend(|| Ok(Box::new(SilentWake))).unwrap();
    wait(|| chat.control.available() && speech.control.available()).await;
    let mut handoff = VoiceHandoff::new();
    handoff.tick_with_tasks(&wake.control, &chat.control, &speech.control, &task.control);
    // Submission comes through a model tool, never through direct Dispatch::Draft.
    chat.control
        .submit("Draft a Beacon handoff for review".into())
        .unwrap();
    wait(|| {
        handoff.tick_with_tasks(&wake.control, &chat.control, &speech.control, &task.control);
        fs::read_to_string(dir.join("spoken")).is_ok_and(|s| s.contains("\nEND\n"))
            && !speech.control.busy()
    })
    .await;
    let spoken = fs::read_to_string(dir.join("spoken")).unwrap();
    assert!(spoken.to_lowercase().contains("review"));
    assert_eq!(spoken.matches("\nEND\n").count(), 1);
    assert!(!dir.join("opened").exists());
    for _ in 0..30 {
        handoff.tick_with_tasks(&wake.control, &chat.control, &speech.control, &task.control);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(fs::read_to_string(dir.join("spoken")).unwrap(), spoken);
    assert_eq!(model.packets().len(), 1);
    assert_eq!(draft_model.packets().len(), 1);
    task.control.shutdown();
    wait(|| task.is_finished()).await;
    task.join().unwrap();
    let mut restored = TaskWorker::with_factory(
        dir.join("context.db"),
        dir.join("reviews"),
        |_, _, _| panic!("recovery must not regenerate"),
        opener,
    )
    .unwrap();
    wait(|| restored.control.snapshot().state == TaskState::ReadyForReview).await;
    let mut restored_handoff = VoiceHandoff::new();
    for _ in 0..20 {
        restored_handoff.tick_with_tasks(
            &wake.control,
            &chat.control,
            &speech.control,
            &restored.control,
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        fs::read_to_string(dir.join("spoken")).unwrap(),
        spoken,
        "startup recovery spoke without a new task completion"
    );
    for (i, alias) in [
        "open result",
        "open the result",
        "review the result",
        "open the file",
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(
            dispatch_question((*alias).into(), &chat.control, &restored.control),
            Ok(Dispatch::Review)
        );
        wait(|| !restored.control.busy()).await;
        assert_eq!(
            fs::read_to_string(dir.join("opened"))
                .unwrap()
                .lines()
                .count(),
            i + 1
        );
    }
    let artifact = restored.control.snapshot().path.unwrap();
    fs::write(artifact, "tampered after completion").unwrap();
    assert_eq!(
        dispatch_question("open the result".into(), &chat.control, &restored.control),
        Ok(Dispatch::Review)
    );
    wait(|| !restored.control.busy()).await;
    assert_eq!(restored.control.snapshot().state, TaskState::Blocked);
    assert_eq!(
        fs::read_to_string(dir.join("opened"))
            .unwrap()
            .lines()
            .count(),
        4,
        "review alias bypassed artifact verification"
    );
    wake.control.shutdown();
    chat.control.stop();
    speech.control.shutdown();
    restored.control.shutdown();
    wait(|| {
        wake.is_finished() && chat.is_finished() && speech.is_finished() && restored.is_finished()
    })
    .await;
    wake.join().unwrap();
    chat.join().unwrap();
    speech.join().unwrap();
    restored.join().unwrap();
}

struct QueuedWake(Arc<Mutex<VecDeque<Vec<f32>>>>);
impl jarvis_panel::wakeworker::WakeBackend for QueuedWake {
    fn start(&mut self) -> Result<(), &'static str> {
        Ok(())
    }
    fn stop(&mut self) {}
    fn poll(&mut self) -> Result<(Vec<f32>, bool), &'static str> {
        Ok((
            self.0.lock().unwrap().pop_front().unwrap_or_default(),
            false,
        ))
    }
    fn transcribe(
        &mut self,
        _: &[f32],
        _: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<String, &'static str> {
        Ok("Jarvis what is next?".into())
    }
}

#[tokio::test]
async fn completed_task_notice_does_not_discard_an_already_queued_voice_question() {
    use jarvis_panel::{
        speech::SpeechWorker,
        voicehandoff::VoiceHandoff,
        wakeworker::{State as WakeState, WakeWorker},
    };
    let dir = TempDir::new();
    seed(&dir);
    let model = Peer::new(vec![answer("Review Beacon next.")]).await;
    let draft_model = Peer::new(vec![answer("Beacon draft.")]).await;
    let endpoint = draft_model.url.clone();
    let mut tasks = TaskWorker::with_factory(
        dir.join("context.db"),
        dir.join("reviews"),
        move |db, j, r| DraftTask::new(db, client(&endpoint), j, r),
        script(&dir, "no-opener", "exit 0"),
    )
    .unwrap();
    let conversation = Conversation::new(dir.join("context.db"), None, model.client());
    let mut chat = ConversationWorker::with_factory(move || Ok(conversation)).unwrap();
    let mut speech =
        SpeechWorker::with_executable(script(&dir, "speaker-priority", "cat > \"${0%/*}/spoken\""))
            .unwrap();
    let queue = Arc::new(Mutex::new(VecDeque::new()));
    let input = queue.clone();
    let mut wake = WakeWorker::with_backend(move || Ok(Box::new(QueuedWake(input)))).unwrap();
    wait(|| chat.control.available() && speech.control.available()).await;
    wake.control.set_enabled(true);
    wait(|| wake.control.snapshot().state == WakeState::WakeListening).await;
    let mut handoff = VoiceHandoff::new();
    handoff.tick_with_tasks(
        &wake.control,
        &chat.control,
        &speech.control,
        &tasks.control,
    );
    tasks
        .control
        .submit("Draft a Beacon handoff".into())
        .unwrap();
    wait(|| !tasks.control.busy() && tasks.control.snapshot().state == TaskState::ReadyForReview)
        .await;
    queue
        .lock()
        .unwrap()
        .extend([vec![0.1; 16000], vec![0.0; 17600]]);
    wait(|| wake.control.snapshot().state == WakeState::CommandReady).await;
    let accepted = handoff.tick_with_tasks(
        &wake.control,
        &chat.control,
        &speech.control,
        &tasks.control,
    );
    assert_eq!(
        accepted.as_deref(),
        Some("what is next?"),
        "task completion took priority and erased queued speech"
    );
    assert!(
        !speech.control.busy(),
        "task notice spoke over the accepted question"
    );
    wait(|| !chat.control.busy()).await;
    assert_eq!(model.packets().len(), 1);
    wake.control.shutdown();
    chat.control.stop();
    speech.control.shutdown();
    tasks.control.shutdown();
    wait(|| {
        wake.is_finished() && chat.is_finished() && speech.is_finished() && tasks.is_finished()
    })
    .await;
    wake.join().unwrap();
    chat.join().unwrap();
    speech.join().unwrap();
    tasks.join().unwrap();
}

#[tokio::test]
async fn cancelling_a_completed_clarification_revokes_its_action_authority() {
    let dir = TempDir::new();
    seed(&dir);
    let model = Peer::new(vec![
        json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[{"id":"question","type":"function","function":{"name":"ask_user","arguments":"{\"question\":\"Which browser?\"}"}}]}}]}),
        json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[{"id":"open","type":"function","function":{"name":"open_application","arguments":"{\"app\":\"Safari\"}"}}]}}]}),
    ]).await;
    let executable = script(
        &dir,
        "app",
        "printf 'effect' > \"${0%/*}/effect\"\nprintf 'verified:running\\n'",
    );
    let apps = AppExecutor::with_executable(
        AppPreferences::default(),
        executable.clone(),
        Duration::from_secs(1),
    )
    .unwrap();
    let runtime = Arc::new(ToolRuntime::read_only(dir.join("context.db")).with_apps(apps));
    let action = ActionExecutor::with_executable(executable, Duration::from_secs(1)).unwrap();
    let conversation = Conversation::new(dir.join("context.db"), None, model.client());
    let mut chat =
        ConversationWorker::with_factory_and_tools(move || Ok(conversation), action, runtime)
            .unwrap();
    wait(|| chat.control.available()).await;
    chat.control.submit("Open the browser".into()).unwrap();
    wait(|| !chat.control.busy()).await;
    assert!(chat.control.snapshot().needs_followup);
    chat.control.cancel();
    chat.control.submit("Safari".into()).unwrap();
    wait(|| !chat.control.busy()).await;
    assert_eq!(model.packets().len(), 2);
    assert!(
        !dir.join("effect").exists(),
        "cancelled question must not authorize a later bare target"
    );
    chat.control.stop();
    wait(|| chat.is_finished()).await;
    chat.join().unwrap();
}
