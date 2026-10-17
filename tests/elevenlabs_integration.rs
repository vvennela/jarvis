//! Real localhost HTTP fixtures only. Audio signatures are synthetic and never played.
use jarvis::{elevenlabs::ElevenLabsClient, providers::ProviderError};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
const KEY: &str = "fixture-eleven-secret-never-log";
const MP3: &str = "ID3\u{4}\0\0\0\0\0\0synthetic-audio-fixture";
struct Peer {
    url: String,
    request: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}
impl Peer {
    fn new(wire: String, delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let request = Arc::new(Mutex::new(None));
        let capture = request.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let handle = thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
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
                socket
                    .set_write_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let n = match socket.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&bytes[..end]);
                        let length = header
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                    assert!(bytes.len() < 100_000);
                }
                *capture.lock().unwrap() = Some(String::from_utf8(bytes).unwrap());
                thread::sleep(delay);
                let _ = socket.write_all(wire.as_bytes());
            }
        });
        Self {
            url,
            request,
            stop,
            handle: Some(handle),
        }
    }
    fn client(&self) -> ElevenLabsClient {
        ElevenLabsClient::loopback_for_tests(KEY.to_owned(), &self.url, Duration::from_secs(2))
            .unwrap()
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.take().unwrap().join().unwrap();
    }
}
fn response(status: &str, kind: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn synthesis_sends_key_header_fixed_flash_model_and_literal_text() {
    let peer = Peer::new(response("200 OK", "audio/mpeg", MP3), Duration::ZERO);
    let client = peer.client();
    let text = "Read this \"exactly\".\nNext line.";
    assert_eq!(client.synthesize(text).await.unwrap(), MP3.as_bytes());
    let request = peer.request.lock().unwrap().clone().unwrap();
    assert!(
        request
            .to_lowercase()
            .contains(&format!("xi-api-key: {KEY}"))
    );
    assert!(!request.to_lowercase().contains("authorization:"));
    assert!(
        request
            .lines()
            .next()
            .unwrap()
            .contains("output_format=mp3_44100_128")
    );
    let body: serde_json::Value =
        serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body["model_id"], "eleven_flash_v2_5");
    assert_eq!(body["text"], text);
    assert!(!body.to_string().contains(KEY));
}

#[tokio::test]
async fn invalid_text_rejected_before_network_and_diagnostics_never_echo_errors() {
    let peer = Peer::new(
        response(
            "401 Unauthorized",
            "application/json",
            &format!("{KEY} PRIVATE_REQUEST"),
        ),
        Duration::ZERO,
    );
    let client = peer.client();
    for text in [
        "".into(),
        " \n".into(),
        "nul\0text".into(),
        "x".repeat(8193),
        "🦀".repeat(2049),
    ] {
        assert!(client.synthesize(&text).await.is_err());
    }
    assert!(peer.request.lock().unwrap().is_none());
    let error = client.synthesize("PRIVATE_REQUEST").await.unwrap_err();
    assert_eq!(error, ProviderError::Authentication);
    let printed = format!("{error} {error:?}");
    assert!(!printed.contains(KEY));
    assert!(!printed.contains("PRIVATE_REQUEST"));
}

#[tokio::test]
async fn redirects_do_not_contact_destination_or_forward_key() {
    let destination = Peer::new(response("200 OK", "audio/mpeg", MP3), Duration::ZERO);
    let source = Peer::new(
        format!(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            destination.url
        ),
        Duration::ZERO,
    );
    assert_eq!(
        source.client().synthesize("hello").await.unwrap_err(),
        ProviderError::Rejected
    );
    assert!(destination.request.lock().unwrap().is_none());
}

#[tokio::test]
async fn non_audio_empty_and_wrong_signature_responses_fail_closed() {
    for (kind, body) in [
        ("application/json", MP3),
        ("audio/mpeg", ""),
        ("audio/mpeg", "<html>secret error</html>"),
        ("audio/wav", "RIFFfake"),
    ] {
        let peer = Peer::new(response("200 OK", kind, body), Duration::ZERO);
        assert_eq!(
            peer.client().synthesize("hello").await.unwrap_err(),
            ProviderError::InvalidResponse
        );
    }
}

#[tokio::test]
async fn declared_and_chunked_audio_are_bounded() {
    let body = format!("ID3{}", "x".repeat(8 * 1024 * 1024));
    for wire in [
        response("200 OK", "audio/mpeg", &body),
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n",
            body.len()
        ),
    ] {
        let peer = Peer::new(wire, Duration::ZERO);
        assert_eq!(
            peer.client().synthesize("hello").await.unwrap_err(),
            ProviderError::ResponseTooLarge
        );
    }
}

#[tokio::test]
async fn stalled_synthesis_respects_deadline() {
    let peer = Peer::new(
        response("200 OK", "audio/mpeg", MP3),
        Duration::from_millis(200),
    );
    let client =
        ElevenLabsClient::loopback_for_tests(KEY.to_owned(), &peer.url, Duration::from_millis(30))
            .unwrap();
    assert_eq!(
        client.synthesize("hello").await.unwrap_err(),
        ProviderError::Timeout
    );
}

#[test]
fn credentials_voice_ids_and_loopback_url_are_validated_without_network() {
    for key in ["", "line\nbreak", "with space"] {
        assert!(ElevenLabsClient::new(key.to_owned(), "fixture-voice").is_err());
    }
    for voice in ["", "../escape", "id?key=secret", "bad voice"] {
        assert!(ElevenLabsClient::new(KEY.to_owned(), voice).is_err());
    }
    for url in [
        "http://example.com/",
        "http://localhost:1234/",
        "file:///tmp/a",
    ] {
        assert!(
            ElevenLabsClient::loopback_for_tests(KEY.to_owned(), url, Duration::from_secs(1))
                .is_err()
        );
    }
}
