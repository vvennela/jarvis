//! Synthetic PCM and route identities only; no hardware or native model access.
use jarvis_voice::capture::{DefaultInputFollower, SilentStreamWatchdog, input_route_token};
use std::time::Duration;

#[test]
fn brief_silence_and_genuinely_quiet_audio_do_not_request_restarts() {
    let mut watchdog = SilentStreamWatchdog::new();
    for cycle in 0..20 {
        let start = cycle * 4_000;
        assert!(!watchdog.observe(&[0.0; 16_000], Duration::from_millis(start)));
        assert!(!watchdog.observe(&[0.0; 16_000], Duration::from_millis(start + 1_000)));
        // Ordinary low-level microphone noise is not a dead digital stream.
        assert!(!watchdog.observe(&[1.0e-12], Duration::from_millis(start + 2_000)));
        assert!(!watchdog.observe(&[], Duration::from_millis(start + 3_000)));
    }
}

#[test]
fn digital_zero_requires_both_audio_duration_and_elapsed_time() {
    let mut watchdog = SilentStreamWatchdog::new();
    assert!(!watchdog.observe(&[0.0; 48_000], Duration::ZERO));
    assert!(
        !watchdog.observe(&[], Duration::from_secs(30)),
        "missing callbacks are not captured zero PCM"
    );
    assert!(watchdog.observe(&[-0.0], Duration::from_secs(30)));

    let mut sparse = SilentStreamWatchdog::new();
    assert!(!sparse.observe(&[0.0], Duration::ZERO));
    assert!(
        !sparse.observe(&[0.0], Duration::from_secs(3)),
        "two samples do not prove three seconds of zero audio"
    );
    assert!(sparse.observe(&[0.0; 48_000], Duration::from_secs(4)));
}

#[test]
fn failed_reopen_and_reset_cannot_create_a_hot_restart_loop() {
    let mut watchdog = SilentStreamWatchdog::new();
    let mut restart_times = Vec::new();
    for second in 0..60 {
        if watchdog.observe(&[0.0; 16_000], Duration::from_secs(second)) {
            restart_times.push(second);
            // The caller clears an interrupted stream even when reopen fails.
            watchdog.reset();
        }
    }
    assert_eq!(restart_times.first(), Some(&3));
    assert!(restart_times.len() <= 6);
    assert!(restart_times.windows(2).all(|pair| pair[1] - pair[0] >= 10));
    assert!(!watchdog.observe(&[0.001; 320], Duration::from_secs(60)));
    assert!(!watchdog.observe(&[0.0; 16_000], Duration::from_secs(61)));
    assert!(!watchdog.observe(&[0.0; 16_000], Duration::from_secs(62)));
}

#[test]
fn clock_rollback_does_not_inherit_a_completed_silence_window() {
    let mut watchdog = SilentStreamWatchdog::new();
    assert!(!watchdog.observe(&[0.0; 48_000], Duration::from_secs(10)));
    assert!(!watchdog.observe(&[0.0; 48_000], Duration::from_secs(9)));
    assert!(!watchdog.observe(&[0.0; 16_000], Duration::from_secs(10)));
    assert!(!watchdog.observe(&[0.0; 16_000], Duration::from_secs(11)));
    assert!(watchdog.observe(&[0.0; 16_000], Duration::from_secs(13)));
}

#[test]
fn same_bluetooth_uid_with_changed_format_is_a_new_route_but_a_pin_stays_pinned() {
    let original = input_route_token("synthetic-headset", 48_000, 2).unwrap();
    let headset_profile = input_route_token("synthetic-headset", 24_000, 1).unwrap();
    let same = input_route_token("synthetic-headset", 48_000, 2).unwrap();
    assert_eq!(original, same);
    assert_ne!(original, headset_profile);
    assert_ne!(
        original,
        input_route_token("synthetic-headset", 48_000, 1).unwrap()
    );
    let mut follower = DefaultInputFollower::new(false, original.clone(), Duration::ZERO);
    assert!(!follower.changed(&same));
    assert!(follower.changed(&headset_profile));
    // An unsuccessful reopen does not commit a candidate route.
    assert_eq!(follower.selected(), original);
    follower.commit(headset_profile.clone());
    assert!(!follower.changed(&headset_profile));
    let pinned = DefaultInputFollower::new(true, original, Duration::ZERO);
    assert!(!pinned.changed(&headset_profile));
    for (id, rate, channels) in [
        ("", 24000, 1),
        ("bad\nuid", 24000, 1),
        ("uid", 0, 1),
        ("uid", 24000, 0),
    ] {
        assert!(input_route_token(id, rate, channels).is_err());
    }
    assert!(input_route_token(&"x".repeat(1025), 24000, 1).is_err());
}

#[test]
fn integer_bit_pattern_subnormals_request_recovery_but_normal_quiet_audio_does_not() {
    let denormals: Vec<f32> = (1..=320).map(f32::from_bits).collect();
    assert!(denormals.iter().all(|value| value.is_subnormal()));
    let mut watchdog = SilentStreamWatchdog::new();
    let mut recovered = false;
    for frame in 0..=150 {
        recovered |= watchdog.observe(&denormals, Duration::from_millis(frame * 20));
    }
    assert!(
        recovered,
        "subnormal PCM from integer-like bit patterns must not leave wake listening stuck forever"
    );

    for sample in [1e-6, -1e-6, f32::MIN_POSITIVE] {
        let mut healthy = SilentStreamWatchdog::new();
        for second in 0..20 {
            assert!(
                !healthy.observe(&[sample; 16000], Duration::from_secs(second)),
                "finite normal quiet PCM must not trigger reconnects"
            );
        }
    }
}

#[test]
fn invalid_float_audio_is_not_misclassified_as_a_recoverable_silent_stream() {
    for sample in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut watchdog = SilentStreamWatchdog::new();
        assert!(!watchdog.observe(&[0.0; 48000], Duration::ZERO));
        assert!(!watchdog.observe(&[sample; 48000], Duration::from_secs(3)));
        assert!(
            !watchdog.observe(&[0.0; 48000], Duration::from_secs(4)),
            "invalid audio must reset any prior zero streak"
        );
    }
}

#[test]
fn restart_storm_budget_survives_resets_and_brief_healthy_batches() {
    let mut watchdog = SilentStreamWatchdog::new();
    let mut requests = Vec::new();
    for second in 0..125 {
        if watchdog.observe(&[f32::from_bits(17); 16000], Duration::from_secs(second)) {
            requests.push(second);
            watchdog.reset();
            assert!(!watchdog.observe(&[0.001], Duration::from_secs(second)));
        }
    }
    assert!(!requests.is_empty());
    for &start in &requests {
        assert!(
            requests
                .iter()
                .filter(|&&at| at >= start && at < start + 60)
                .count()
                <= 3,
            "more than three recovery attempts in a rolling minute: {requests:?}"
        );
    }
    assert!(
        requests.iter().any(|&at| at >= 60),
        "bounded retry should recover eligibility after cooldown"
    );
}

#[test]
fn same_device_rate_and_channels_with_a_different_sample_representation_changes_route() {
    use jarvis_voice::capture::input_route_token_with_format;
    let float = input_route_token_with_format("headset-fixture", 24000, 1, "F32").unwrap();
    let integer = input_route_token_with_format("headset-fixture", 24000, 1, "I16").unwrap();
    assert_ne!(float, integer);
    let mut follower = DefaultInputFollower::new(false, float.clone(), Duration::ZERO);
    assert!(follower.changed(&integer));
    assert_eq!(
        follower.selected(),
        float,
        "failed reopen must not commit a candidate format"
    );
    follower.commit(integer.clone());
    assert!(!follower.changed(&integer));
    for format in ["", "F32\n", "F32:I16", "float sample"] {
        assert!(input_route_token_with_format("headset-fixture", 24000, 1, format).is_err());
    }
    assert!(input_route_token_with_format("headset-fixture", 24000, 1, &"x".repeat(33)).is_err());
}

#[test]
fn runtime_reconnect_waits_before_each_attempt_and_stops_after_three_failures() {
    use jarvis_voice::capture::ReconnectBackoff;
    let mut retry = ReconnectBackoff::new();
    assert!(!retry.due(Duration::from_secs(10)));
    assert!(!retry.failed(Duration::ZERO));
    assert!(retry.begin(Duration::ZERO));
    assert!(retry.begin(Duration::from_millis(249)));
    assert!(!retry.due(Duration::from_millis(249)));
    assert!(
        retry.failed(Duration::from_millis(249)),
        "early failure must not consume the first attempt"
    );
    assert!(retry.due(Duration::from_millis(250)));
    assert!(retry.failed(Duration::from_millis(250)));
    assert!(!retry.due(Duration::from_millis(749)));
    assert!(retry.due(Duration::from_millis(750)));
    assert!(retry.failed(Duration::from_millis(750)));
    assert!(!retry.due(Duration::from_millis(1749)));
    assert!(retry.due(Duration::from_millis(1750)));
    assert!(!retry.failed(Duration::from_millis(1750)));
    for now in [2000, 10000, 60000] {
        assert!(!retry.begin(Duration::from_millis(now)));
        assert!(!retry.due(Duration::from_millis(now)));
    }
}

#[test]
fn a_confirmed_healthy_session_clears_old_deadlines_without_an_immediate_reopen() {
    use jarvis_voice::capture::ReconnectBackoff;
    let mut retry = ReconnectBackoff::new();
    assert!(retry.begin(Duration::ZERO));
    assert!(retry.failed(Duration::from_millis(250)));
    retry.success();
    assert!(!retry.due(Duration::from_secs(20)));
    assert!(retry.begin(Duration::from_secs(20)));
    assert!(!retry.due(Duration::from_millis(20249)));
    assert!(retry.due(Duration::from_millis(20250)));
    assert!(retry.failed(Duration::from_millis(20250)));
}

#[test]
fn builtin_fallback_prefers_trusted_transport_identity_over_names_or_default_order() {
    use jarvis_voice::capture::select_builtin_input_name;
    let candidates = vec![
        ("AirPods Pro".into(), false),
        ("MacBook Pro Microphone".into(), false),
        ("Fixture built-in input".into(), true),
    ];
    assert_eq!(
        select_builtin_input_name(&candidates).unwrap(),
        "Fixture built-in input"
    );
    let reversed: Vec<_> = candidates.into_iter().rev().collect();
    assert_eq!(
        select_builtin_input_name(&reversed).unwrap(),
        "Fixture built-in input"
    );
}

#[test]
fn builtin_fallback_does_not_guess_when_transport_or_names_are_ambiguous() {
    use jarvis_voice::capture::select_builtin_input_name;
    for candidates in [
        vec![],
        vec![
            ("AirPods Pro".into(), false),
            ("USB Studio Mic".into(), false),
        ],
        vec![
            ("First internal".into(), true),
            ("Second internal".into(), true),
        ],
        vec![
            ("MacBook Pro Microphone".into(), false),
            ("MacBook Pro Microphone".into(), false),
        ],
        vec![("MacBook Pro Microphone replica".into(), false)],
    ] {
        assert!(
            select_builtin_input_name(&candidates).is_err(),
            "ambiguous/nonbuiltin input must not silently become a fallback: {candidates:?}"
        );
    }
    assert_eq!(
        select_builtin_input_name(&[("macbook pro microphone".into(), false)]).unwrap(),
        "macbook pro microphone"
    );
}

#[test]
fn builtin_fallback_metadata_is_bounded_and_cannot_inject_labels() {
    use jarvis_voice::capture::select_builtin_input_name;
    for name in [String::new(), "private\nforged".into(), "x".repeat(129)] {
        assert!(select_builtin_input_name(&[(name, true)]).is_err());
    }
    let too_many = vec![("Fixture".into(), false); 33];
    assert!(select_builtin_input_name(&too_many).is_err());
}

#[test]
fn healthy_capture_requires_real_normal_samples_and_elapsed_time_despite_empty_polls() {
    use jarvis_voice::capture::CaptureProgress;
    let mut health = CaptureProgress::new();
    health.on_start(Duration::ZERO);
    for frame in 0..100 {
        let now = Duration::from_millis(frame * 20);
        assert!(!health.observe(&[1e-6; 320], now));
        assert!(!health.observe(&[], now + Duration::from_millis(10)));
    }
    assert!(
        health.observe(&[1e-6; 320], Duration::from_secs(2)),
        "empty polling gaps must not prevent a healthy stream from resetting its retry budget"
    );
    let mut insufficient = CaptureProgress::new();
    insufficient.on_start(Duration::ZERO);
    assert!(!insufficient.observe(&[0.1; 320], Duration::ZERO));
    assert!(
        !insufficient.observe(&[0.1; 320], Duration::from_secs(2)),
        "elapsed time with too few samples is not proof of sustained healthy capture"
    );
}

#[test]
fn capture_progress_distinguishes_no_callbacks_from_received_silence() {
    use jarvis_voice::capture::CaptureProgress;
    let mut health = CaptureProgress::new();
    health.on_start(Duration::from_secs(10));
    assert!(!health.no_arrivals(Duration::from_millis(12999)));
    assert!(health.no_arrivals(Duration::from_secs(13)));
    assert!(!health.observe(&[], Duration::from_secs(13)));
    assert!(health.no_arrivals(Duration::from_secs(13)));
    assert!(!health.observe(&[0.0; 320], Duration::from_secs(13)));
    assert!(!health.no_arrivals(Duration::from_secs(15)));
    assert!(health.no_arrivals(Duration::from_secs(16)));
}

#[test]
fn broken_or_nonfinite_batches_revoke_partial_health_until_new_sustained_normal_audio() {
    use jarvis_voice::capture::CaptureProgress;
    for bad in [0.0, f32::from_bits(71), f32::NAN, f32::INFINITY] {
        let mut health = CaptureProgress::new();
        health.on_start(Duration::ZERO);
        assert!(!health.observe(&[0.1; 16000], Duration::ZERO));
        assert!(!health.observe(&[bad; 320], Duration::from_secs(1)));
        assert!(!health.observe(&[0.1; 32000], Duration::from_secs(2)));
        assert!(!health.observe(&[], Duration::from_secs(3)));
        assert!(health.observe(&[0.1; 320], Duration::from_secs(4)));
    }
}

#[test]
fn fallback_hold_uses_physical_identity_without_confusing_format_drift_with_device_change() {
    use jarvis_voice::capture::{input_route_device_id, input_route_token_with_format};
    let physical = "fixture:Bluetooth:héádset";
    for (rate, channels, format) in [(24000, 1, "F32"), (48000, 2, "I16"), (16000, 1, "F32")] {
        let token = input_route_token_with_format(physical, rate, channels, format).unwrap();
        assert_eq!(input_route_device_id(&token).unwrap(), physical);
    }
    let first = input_route_token(physical, 24000, 1).unwrap();
    let second = input_route_token("different-headset", 24000, 1).unwrap();
    assert_ne!(
        input_route_device_id(&first).unwrap(),
        input_route_device_id(&second).unwrap()
    );
    for token in [
        "",
        "garbage",
        "999:short:24000:1:F32",
        "3:abc:0:1:F32",
        "3:abc:24000:0:F32",
        "3:abc:24000:1:F32:extra",
        "1:é:24000:1:F32",
    ] {
        assert!(
            input_route_device_id(token).is_err(),
            "malformed token cannot become fallback identity: {token:?}"
        );
    }
}
