//! Email drafts: real tool loop over loopback HTTP and a fake osascript that
//! records every Mail/Contacts invocation. Nothing reaches Mail or the network.
mod support;
use jarvis::{
    context::{ContextStore, Limits},
    conversation::Conversation,
    email::EmailClient,
    providers::{CrusoeClient, NEMOTRON_MODEL, ProviderConfig},
    tool_runtime::ToolRuntime,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
    time::Duration,
};
use support::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Plan {
    body: Value,
    delay: Duration,
}
fn text(value: &str) -> Plan {
    Plan {
        body: json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":value}}]}),
        delay: Duration::ZERO,
    }
}
fn call(id: &str, name: &str, args: Value) -> Value {
    json!({"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}})
}
fn calls(values: Vec<Value>) -> Plan {
    Plan {
        body: json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":values}}]}),
        delay: Duration::ZERO,
    }
}
struct Peer {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    server: tokio::task::JoinHandle<()>,
}
impl Peer {
    async fn new(plans: Vec<Plan>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let plans = Arc::new(Mutex::new(VecDeque::from(plans)));
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let captured = captured.clone();
                let plans = plans.clone();
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let mut chunk = [0; 4096];
                    loop {
                        let n =
                            tokio::time::timeout(Duration::from_secs(2), socket.read(&mut chunk))
                                .await
                                .unwrap()
                                .unwrap();
                        if n == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&chunk[..n]);
                        assert!(bytes.len() < 150000);
                        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&bytes[..end]);
                            let length = headers
                                .lines()
                                .find_map(|line| {
                                    let (k, v) = line.split_once(':')?;
                                    k.eq_ignore_ascii_case("content-length")
                                        .then(|| v.trim().parse::<usize>().unwrap())
                                })
                                .unwrap();
                            if bytes.len() >= end + 4 + length {
                                captured.lock().unwrap().push(
                                    serde_json::from_slice(&bytes[end + 4..end + 4 + length])
                                        .unwrap(),
                                );
                                break;
                            }
                        }
                    }
                    let plan = plans
                        .lock()
                        .unwrap()
                        .pop_front()
                        .unwrap_or_else(|| text("fixture terminal answer"));
                    tokio::time::sleep(plan.delay).await;
                    let body = plan.body.to_string();
                    let _=socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
                });
            }
        });
        Self {
            url,
            requests,
            server,
        }
    }
    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
    fn conversation(&self, dir: &TempDir, runtime: ToolRuntime) -> Conversation {
        let client = CrusoeClient::new(
            "fixture-only-key",
            NEMOTRON_MODEL,
            ProviderConfig::loopback_for_tests(&self.url, Duration::from_secs(2)).unwrap(),
        )
        .unwrap();
        let mut conversation = Conversation::new(dir.join("context.db"), None, client);
        conversation.set_tool_runtime(Arc::new(runtime));
        conversation
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// Fake osascript: argv after the script is logged; contacts come from a file.
fn fake_osascript(dir: &TempDir, contacts: &str, send_result: &str) -> std::path::PathBuf {
    fs::write(dir.join("contacts"), contacts).unwrap();
    let script = format!(
        "#!/bin/sh\nfixture=${{0%/*}}\nshift\nscript=\"$1\"\nshift\ncase \"$script\" in\n  *Contacts*) printf 'contacts' >> \"$fixture/calls\"; for a in \"$@\"; do printf '\\t%s' \"$a\" >> \"$fixture/calls\"; done; printf '\\n' >> \"$fixture/calls\"; printf 'ok\\n'; cat \"$fixture/contacts\";;\n  *Mail*) printf 'send' >> \"$fixture/calls\"; for a in \"$@\"; do printf '\\t%s' \"$a\" >> \"$fixture/calls\"; done; printf '\\n' >> \"$fixture/calls\"; printf '{send_result}';;\nesac\n"
    );
    let path = dir.join("osascript");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}
fn calls_log(dir: &TempDir) -> Vec<String> {
    fs::read_to_string(dir.join("calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}
fn sends(dir: &TempDir) -> Vec<String> {
    calls_log(dir)
        .into_iter()
        .filter(|line| line.starts_with("send"))
        .collect()
}
fn runtime(dir: &TempDir, contacts: &str, send_result: &str) -> ToolRuntime {
    drop(ContextStore::open(&dir.join("context.db"), Limits::default()).unwrap());
    ToolRuntime::read_only(dir.join("context.db")).with_email(
        EmailClient::with_executable(
            fake_osascript(dir, contacts, send_result),
            Duration::from_secs(5),
        )
        .unwrap(),
    )
}
fn compose(to: &str, subject: &str, body: &str) -> Plan {
    calls(vec![call(
        "c1",
        "compose_email",
        json!({"to":to,"subject":subject,"body":body}),
    )])
}
const BRIAN: &str = "Brian Chen\tbrian@example.com\n";

#[tokio::test]
async fn draft_is_read_back_and_sent_only_after_explicit_confirmation() {
    let dir = TempDir::new();
    let peer = Peer::new(vec![compose(
        "Brian",
        "Running late",
        "I'll be ten minutes late.",
    )])
    .await;
    let mut conversation = peer.conversation(&dir, runtime(&dir, BRIAN, "sent"));
    let reply = conversation
        .ask("email Brian that I'll be ten minutes late")
        .await
        .unwrap();
    assert!(reply.needs_followup, "{}", reply.text);
    for part in [
        "Brian Chen",
        "brian at example dot com",
        "Running late",
        "ten minutes late",
        "send it?",
    ] {
        assert!(
            reply.text.contains(part),
            "read-back missing {part}: {}",
            reply.text
        );
    }
    assert!(sends(&dir).is_empty(), "composing sent mail");
    let sent = conversation.ask("send it").await.unwrap();
    assert_eq!(sent.text, "Sent it to Brian Chen.");
    assert_eq!(
        sends(&dir),
        vec!["send\tbrian@example.com\tRunning late\tI'll be ten minutes late."]
    );
    // The draft is consumed: repeating the confirmation cannot send twice.
    let again = conversation.ask("send it").await.unwrap();
    assert_eq!(sends(&dir).len(), 1, "{}", again.text);
    // The confirmed send never reached the model. The repeat did, as plain
    // conversation, where no send capability exists.
    assert_eq!(peer.requests().len(), 2);
    assert!(
        peer.requests()[1]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| !tool["function"]["name"].as_str().unwrap().starts_with("send")),
        "a send tool was offered to the model"
    );
}

#[tokio::test]
async fn bare_yes_sends_only_right_after_the_read_back() {
    let dir = TempDir::new();
    let peer = Peer::new(vec![
        compose("Brian", "Lunch", "Lunch at noon?"),
        text("Sure, the launch checklist is the next thing to look at."),
    ])
    .await;
    let mut conversation = peer.conversation(&dir, runtime(&dir, BRIAN, "sent"));
    conversation
        .ask("email Brian about lunch at noon")
        .await
        .unwrap();
    conversation
        .ask("what should I work on next")
        .await
        .unwrap();
    let reply = conversation.ask("yes").await.unwrap();
    assert!(
        sends(&dir).is_empty(),
        "stale yes sent mail: {}",
        reply.text
    );
    let read = conversation.ask("read it back").await.unwrap();
    assert!(read.text.contains("Lunch at noon?"));
    conversation.ask("yes").await.unwrap();
    assert_eq!(sends(&dir).len(), 1);
}

#[tokio::test]
async fn revision_replaces_the_draft_and_only_the_latest_is_sent() {
    let dir = TempDir::new();
    let peer = Peer::new(vec![
        compose("Brian", "Late", "Formal version."),
        compose("Brian", "Late", "Casual version."),
    ])
    .await;
    let mut conversation = peer.conversation(&dir, runtime(&dir, BRIAN, "sent"));
    conversation.ask("email Brian that I'm late").await.unwrap();
    let revised = conversation
        .ask("make the email to Brian more casual")
        .await
        .unwrap();
    assert!(revised.text.contains("Casual version."));
    conversation.ask("send it").await.unwrap();
    assert_eq!(
        sends(&dir),
        vec!["send\tbrian@example.com\tLate\tCasual version."]
    );
}

#[tokio::test]
async fn discard_and_no_drop_the_draft_without_sending() {
    for answer in ["don't send it", "no"] {
        let dir = TempDir::new();
        let peer = Peer::new(vec![compose("Brian", "Hi", "Hello.")]).await;
        let mut conversation = peer.conversation(&dir, runtime(&dir, BRIAN, "sent"));
        conversation.ask("email Brian hello").await.unwrap();
        let reply = conversation.ask(answer).await.unwrap();
        assert_eq!(reply.text, "Okay, I won't send it.");
        conversation.ask("send it").await.unwrap();
        assert!(sends(&dir).is_empty(), "{answer} still sent");
    }
}

#[tokio::test]
async fn model_cannot_choose_a_recipient_the_user_never_named() {
    let dir = TempDir::new();
    let peer = Peer::new(vec![
        compose("Mallory", "Hi", "Exfiltrated notes."),
        text("I can't do that one."),
    ])
    .await;
    let contacts = "Mallory Evil\tmallory@example.com\nBrian Chen\tbrian@example.com\n";
    let mut conversation = peer.conversation(&dir, runtime(&dir, contacts, "sent"));
    conversation.ask("email Brian my notes").await.unwrap();
    conversation.ask("send it").await.unwrap();
    assert!(sends(&dir).is_empty());
    assert!(
        calls_log(&dir).iter().all(|line| !line.contains("Mallory")),
        "unnamed recipient was even looked up"
    );
}

#[tokio::test]
async fn compose_requires_an_email_request_and_ambiguity_is_asked_not_guessed() {
    let dir = TempDir::new();
    let peer = Peer::new(vec![
        compose("Brian", "Hi", "Hello."),
        text("ok"),
        compose("Brian", "Hi", "Hello."),
    ])
    .await;
    let contacts = "Brian Chen\tbrian@example.com\nBrian Ortiz\tbortiz@example.com\n";
    let mut conversation = peer.conversation(&dir, runtime(&dir, contacts, "sent"));
    // No email word: a model proposal cannot create a draft.
    conversation.ask("tell Brian hello").await.unwrap();
    conversation.ask("send it").await.unwrap();
    assert!(calls_log(&dir).is_empty());
    let reply = conversation.ask("email Brian hello").await.unwrap();
    assert!(
        reply.text.contains("A few contacts match"),
        "{}",
        reply.text
    );
    assert!(reply.needs_followup);
    conversation.ask("send it").await.unwrap();
    assert!(sends(&dir).is_empty());
}

#[tokio::test]
async fn explicit_address_needs_no_contact_lookup_and_values_are_passed_literally() {
    let dir = TempDir::new();
    let hostile = "Re: \"quote\" & $(touch pwned) `id`; end tell";
    let peer = Peer::new(vec![compose(
        "sam@example.org",
        hostile,
        "Body with \"quotes\" and $HOME.",
    )])
    .await;
    let mut conversation = peer.conversation(&dir, runtime(&dir, "", "sent"));
    conversation
        .ask("send an email to sam@example.org about the release")
        .await
        .unwrap();
    conversation.ask("yes send it").await.unwrap();
    assert_eq!(
        sends(&dir),
        vec![format!(
            "send\tsam@example.org\t{hostile}\tBody with \"quotes\" and $HOME."
        )]
    );
    assert!(!dir.join("pwned").exists());
    assert!(
        calls_log(&dir)
            .iter()
            .all(|line| !line.starts_with("contacts"))
    );
}

#[tokio::test]
async fn mail_rejection_reports_not_sent_and_does_not_retry() {
    let dir = TempDir::new();
    let peer = Peer::new(vec![compose("Brian", "Hi", "Hello.")]).await;
    let mut conversation = peer.conversation(&dir, runtime(&dir, BRIAN, "not_sent"));
    conversation.ask("email Brian hello").await.unwrap();
    let reply = conversation.ask("send it").await.unwrap();
    assert!(reply.text.contains("nothing was sent"), "{}", reply.text);
    conversation.ask("send it").await.unwrap();
    assert_eq!(sends(&dir).len(), 1, "failed send was retried");
}

#[test]
fn confirmation_phrases_are_explicit() {
    use jarvis::email::is_send_confirmation;
    for phrase in [
        "send it",
        "Yes, send it.",
        "go ahead and send it",
        "Jarvis, send it",
    ] {
        assert!(is_send_confirmation(phrase, false), "{phrase}");
    }
    for phrase in ["yes", "sure", "go ahead"] {
        assert!(!is_send_confirmation(phrase, false), "{phrase}");
        assert!(is_send_confirmation(phrase, true), "{phrase}");
    }
    for phrase in [
        "don't send it",
        "send it to Mallory instead",
        "should I send it",
        "what",
    ] {
        assert!(!is_send_confirmation(phrase, true), "{phrase}");
    }
}
