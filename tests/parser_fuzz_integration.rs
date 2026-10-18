//! Every text parser on the voice/typed request path must survive arbitrary
//! recognizer output without panicking: curly quotes, accents whose case
//! changes byte length, emoji, combining marks and zero-width characters.
mod support;
use jarvis::{
    email::{context_category, is_discard_request, is_read_back_request, is_send_confirmation},
    experience::ExperienceStore,
    messages::{attributed_text, mentions_messages, normalize_handle},
    preferences::PreferenceStore,
    providers::ToolCall,
    request_intent::{
        canonical_request, command_clauses, generic_music_request, negated, negated_before,
        resolve_action_followup, resolved_action, tab_navigation_intent, tab_switch_intent,
        wikipedia_page_intent,
    },
    spotify_playback::requested_artist,
    web_visit::website_intent,
};
use serde_json::json;
use support::TempDir;

const PIECES: &[&str] = &[
    "open",
    "play",
    "the",
    "Wikipedia",
    "page",
    "for",
    "Napoleon",
    "tab",
    "and",
    ",",
    "email",
    "Brian",
    "send it",
    "don’t",
    "don't",
    "I'm",
    "’",
    "‘",
    "“",
    "”",
    "İ",
    "ẞ",
    "ﬀ",
    "é",
    "Ö",
    "😀",
    "👍🏽",
    "e\u{301}",
    "\u{200b}",
    "\u{feff}",
    "cnn.com",
    "dot",
    "com",
    "https://",
    "@",
    "Hey",
    "Jarvis",
    "to",
    "pull up",
    "some music",
    "Kanye",
    "please",
    "if",
    "then",
    "texts",
    "remember",
    "forget",
    "never",
    " ",
    "  ",
    ".",
    "?",
    "!",
    "-",
    "'s",
    "日本",
    "Ωmega",
    "ǅ",
];

/// Deterministic xorshift so failures reproduce.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn text(&mut self) -> String {
        let n = (self.next() % 14) as usize;
        let mut out = String::new();
        for _ in 0..n {
            let piece = PIECES[(self.next() as usize) % PIECES.len()];
            if self.next() % 3 == 0 {
                out.push_str(&piece.to_uppercase());
            } else {
                out.push_str(piece);
            }
            if self.next() % 2 == 0 {
                out.push(' ');
            }
        }
        out
    }
}

#[test]
fn request_parsers_never_panic_on_arbitrary_recognizer_text() {
    let dir = TempDir::new();
    let preferences = PreferenceStore::new(dir.join("preferences.json"));
    let experience = ExperienceStore::new(dir.join("experience.json"));
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..20_000 {
        let a = rng.text();
        let b = rng.text();
        let _ = canonical_request(&a);
        let _ = command_clauses(&a);
        let _ = wikipedia_page_intent(&a);
        let _ = tab_switch_intent(&a);
        let _ = tab_navigation_intent(&a);
        let _ = website_intent(&a);
        let _ = generic_music_request(&a);
        let _ = resolved_action(&a);
        let _ = negated(&a);
        let _ = negated_before(&a, &["email", "texts"]);
        let _ = resolve_action_followup(&a, &b);
        let _ = requested_artist(&a);
        let _ = mentions_messages(&a);
        let _ = context_category(&a);
        let _ = is_send_confirmation(&a, true);
        let _ = is_read_back_request(&a);
        let _ = is_discard_request(&a);
        let _ = normalize_handle(&a);
        let _ = attributed_text(a.as_bytes());
        let _ = jarvis_panel_free_classifiers(&a);
        let _ = preferences.remember(&a, &b);
        let _ = preferences.forget(&a, &b);
        let _ = experience.match_request(&a);
        experience.record_model_turn(
            &a,
            Some(&ToolCall {
                id: "f".into(),
                name: "spotify_play_artist".into(),
                arguments: json!({ "artist": b }),
            }),
            std::time::Duration::from_millis(1),
        );
    }
}

/// The hands-free classifier lives in the panel crate; mirror its word logic
/// here through the shared parsers it relies on.
fn jarvis_panel_free_classifiers(text: &str) -> bool {
    canonical_request(text).is_some() || negated(text)
}
