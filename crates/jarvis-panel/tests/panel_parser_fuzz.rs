//! Panel-side text handling survives arbitrary recognizer output.
use jarvis_panel::{
    voicehandoff::{classify_hands_free, is_filler},
    wake::{WakeDetector, wake_tail},
};

const PIECES: &[&str] = &[
    "Jarvis",
    "hey",
    "thanks",
    "send it",
    "um",
    "don’t",
    "’",
    "İ",
    "ẞ",
    "😀",
    "e\u{301}",
    "\u{200b}",
    "yes",
    ",",
    ".",
    "  ",
    "日本",
    "ǅ",
    "stop",
    "that's all",
    "Kanye",
];

#[test]
fn panel_text_handling_never_panics() {
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..20_000 {
        let n = (next() % 10) as usize;
        let mut text = String::new();
        for _ in 0..n {
            let piece = PIECES[(next() as usize) % PIECES.len()];
            if next() % 3 == 0 {
                text.push_str(&piece.to_uppercase());
            } else {
                text.push_str(piece);
            }
            if next() % 2 == 0 {
                text.push(' ');
            }
        }
        let _ = classify_hands_free(&text);
        let _ = is_filler(&text);
        let _ = wake_tail(&text);
        let mut detector = WakeDetector::new();
        detector.arm_keyword(&[0.1; 1600]);
        if let Some(request) = detector.ingest(&vec![0.0; 16_000 * 2]) {
            assert!(request.skip_before.is_some());
            let _ = detector.accept_transcript(&text);
        }
    }
}
