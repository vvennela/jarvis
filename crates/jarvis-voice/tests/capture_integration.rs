//! Pure callback-buffer integration. These tests never open an input device.
use jarvis_voice::capture::{CaptureBuffer, MAX_RECORDING_SAMPLES};

#[test]
fn capture_format_validation_rejects_unbounded_or_invalid_device_shapes() {
    for (rate, channels) in [(0, 1), (7999, 1), (192001, 1), (16000, 0), (16000, 9)] {
        assert!(CaptureBuffer::new(rate, channels).is_err());
    }
    for (rate, channels) in [(8000, 1), (16000, 2), (44100, 2), (48000, 8), (192000, 1)] {
        assert!(CaptureBuffer::new(rate, channels).is_ok());
    }
    assert!(CaptureBuffer::new(16000, 1).unwrap().finish().is_err());
}

#[test]
fn stereo_downmix_and_sample_format_normalization_produce_valid_mono() {
    let mut stereo = CaptureBuffer::new(16000, 2).unwrap();
    stereo.push_f32(&[0.75, -0.75].repeat(100)).unwrap();
    let output = stereo.finish().unwrap();
    assert!(!output.is_empty());
    assert!(output.iter().all(|sample| sample.abs() < 0.00001));
    let mut signed = CaptureBuffer::new(16000, 1).unwrap();
    signed.push_i16(&[i16::MIN, 0, i16::MAX, 0]).unwrap();
    let samples = signed.finish().unwrap();
    assert_eq!(samples[0], -1.0);
    assert_eq!(samples[1], 0.0);
    assert!(samples[2] > 0.99 && samples[2] <= 1.0);
    let mut unsigned = CaptureBuffer::new(16000, 1).unwrap();
    unsigned.push_u16(&[0, 32768, u16::MAX, 32768]).unwrap();
    let samples = unsigned.finish().unwrap();
    assert_eq!(samples[0], -1.0);
    assert!(samples[1].abs() < 0.0001);
    assert!(samples[2] > 0.99 && samples[2] <= 1.0);
}

#[test]
fn channel_fragments_and_arbitrary_callback_boundaries_do_not_change_resampling() {
    for rate in [8000, 16000, 44100, 48000, 192000] {
        let input: Vec<f32> = (0..1000)
            .flat_map(|i| {
                let value = (i as f32 / 1000.0) - 0.5;
                [value, value * 0.5]
            })
            .collect();
        let mut whole = CaptureBuffer::new(rate, 2).unwrap();
        whole.push_f32(&input).unwrap();
        let expected = whole.finish().unwrap();
        let mut fragmented = CaptureBuffer::new(rate, 2).unwrap();
        let mut index = 0;
        for length in [1, 7, 2, 9, 3, 15].into_iter().cycle() {
            if index >= input.len() {
                break;
            }
            let end = (index + length).min(input.len());
            fragmented.push_f32(&input[index..end]).unwrap();
            index = end;
        }
        let actual = fragmented.finish().unwrap();
        assert_eq!(
            actual, expected,
            "callback boundaries changed output at {rate}Hz"
        );
        let approximate = 1000.0 * 16000.0 / f64::from(rate);
        assert!((actual.len() as f64 - approximate).abs() <= 3.0);
        assert!(
            actual
                .iter()
                .all(|sample| sample.is_finite() && (-1.0..=1.0).contains(sample))
        );
    }
}

#[test]
fn invalid_floats_are_rejected_instead_of_entering_transcription() {
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut buffer = CaptureBuffer::new(16000, 1).unwrap();
        assert!(buffer.push_f32(&[value]).is_err());
    }
}

#[test]
fn long_recordings_stop_growing_at_the_fifteen_second_cap() {
    let mut buffer = CaptureBuffer::new(16000, 1).unwrap();
    let _ = buffer.push_f32(&vec![0.25; MAX_RECORDING_SAMPLES + 100]);
    assert!(buffer.is_full());
    assert!(buffer.output_len() <= MAX_RECORDING_SAMPLES);
    let before = buffer.output_len();
    for _ in 0..100 {
        let _ = buffer.push_f32(&[0.5; 1000]);
        assert_eq!(buffer.output_len(), before);
    }
    let samples = buffer.finish().unwrap();
    assert_eq!(samples.len(), MAX_RECORDING_SAMPLES);
    assert!(samples.iter().all(|sample| *sample == 0.25));
}

#[test]
fn continuous_capture_drains_without_losing_resampler_position_and_marks_overrun() {
    let input: Vec<f32> = (0..48000).map(|i| (i % 100) as f32 / 100.0).collect();
    let mut baseline = CaptureBuffer::continuous(48000, 1).unwrap();
    baseline.push_f32(&input).unwrap();
    let (expected, overflow) = baseline.drain_samples();
    assert!(!overflow);
    let mut streamed = CaptureBuffer::continuous(48000, 1).unwrap();
    let mut actual = Vec::new();
    for chunk in input.chunks(71) {
        streamed.push_f32(chunk).unwrap();
        let (samples, overflow) = streamed.drain_samples();
        assert!(!overflow);
        actual.extend(samples);
    }
    assert_eq!(
        actual, expected,
        "draining must preserve interpolation continuity"
    );
    let mut buffer = CaptureBuffer::continuous(16000, 1).unwrap();
    buffer
        .push_f32(&vec![0.1; MAX_RECORDING_SAMPLES * 3])
        .unwrap();
    assert!(
        !buffer.is_full(),
        "continuous input must remain usable after 15 seconds"
    );
    assert!(buffer.output_len() <= MAX_RECORDING_SAMPLES);
    let (samples, overflow) = buffer.drain_samples();
    assert!(overflow);
    assert!(!samples.is_empty());
    assert_eq!(buffer.drain_samples(), (Vec::new(), false));
    buffer.push_f32(&[0.25; 320]).unwrap();
    let (samples, overflow) = buffer.drain_samples();
    assert!(!overflow);
    assert_eq!(samples, vec![0.25; 320]);
}

#[cfg(target_os = "macos")]
#[test]
fn callback_xrun_and_route_change_invalidate_one_utterance_without_disabling_capture() {
    use jarvis_voice::capture::CaptureHealth;
    use std::sync::Arc;
    let health = Arc::new(CaptureHealth::default());
    assert!(!health.failed());
    assert!(!health.take_discontinuity());
    // The actual CPAL callback calls this same health API. No device is opened.
    let callback = health.clone();
    std::thread::spawn(move || {
        callback.record_stream_error(cpal::ErrorKind::Xrun);
        callback.record_stream_error(cpal::ErrorKind::DeviceChanged);
    })
    .join()
    .unwrap();
    assert!(
        !health.failed(),
        "recoverable callback notification must not permanently stop the stream"
    );
    assert!(
        health.take_discontinuity(),
        "consumer must discard interrupted utterance"
    );
    assert!(
        !health.take_discontinuity(),
        "one interruption cannot poison every future utterance"
    );
    health.record_stream_error(cpal::ErrorKind::RealtimeDenied);
    assert!(!health.failed());
    assert!(!health.take_discontinuity());
    health.record_stream_error(cpal::ErrorKind::Xrun);
    assert!(health.take_discontinuity());
    assert!(!health.failed());
}

#[cfg(target_os = "macos")]
#[test]
fn fatal_capture_errors_remain_failed_after_later_recoverable_notifications() {
    use jarvis_voice::capture::CaptureHealth;
    for fatal in [
        cpal::ErrorKind::DeviceNotAvailable,
        cpal::ErrorKind::PermissionDenied,
        cpal::ErrorKind::StreamInvalidated,
        cpal::ErrorKind::BackendError,
        cpal::ErrorKind::Other,
    ] {
        let health = CaptureHealth::default();
        health.record_stream_error(fatal);
        assert!(health.failed());
        health.record_stream_error(cpal::ErrorKind::Xrun);
        health.record_stream_error(cpal::ErrorKind::DeviceChanged);
        health.record_stream_error(cpal::ErrorKind::RealtimeDenied);
        assert!(
            health.failed(),
            "recoverable notification resurrected fatal stream: {fatal:?}"
        );
        assert!(health.take_discontinuity());
        assert!(health.failed());
    }
}

#[test]
fn explicit_microphone_selection_is_exact_unique_and_never_falls_back_silently() {
    use jarvis_voice::capture::select_input_index;
    let names = vec![
        "MacBook Pro Microphone".to_owned(),
        "AirPods Pro".to_owned(),
        "USB Audio".to_owned(),
    ];
    assert_eq!(select_input_index(&names, "AirPods Pro").unwrap(), 1);
    assert_eq!(
        select_input_index(&names, "MacBook Pro Microphone").unwrap(),
        0
    );
    for requested in [
        "AirPods",
        "airpods pro",
        "Unknown device",
        "",
        " ",
        "AirPods Pro\0",
    ] {
        assert!(
            select_input_index(&names, requested).is_err(),
            "invalidorambiguousselection silently chose a device: {requested:?}"
        );
    }
    assert!(select_input_index(&[], "AirPods Pro").is_err());
    let duplicate = vec!["AirPods Pro".to_owned(), "AirPods Pro".to_owned()];
    assert!(
        select_input_index(&duplicate, "AirPods Pro").is_err(),
        "duplicate exactnames require clarification"
    );
}

#[test]
fn microphone_inventory_and_utf8_names_have_explicit_resource_bounds() {
    use jarvis_voice::capture::select_input_index;
    let names: Vec<_> = (0..32).map(|i| format!("Fixture microphone {i}")).collect();
    assert_eq!(
        select_input_index(&names, "Fixture microphone 31").unwrap(),
        31
    );
    let mut overflow = names;
    overflow.push("extra microphone".into());
    assert!(select_input_index(&overflow, "Fixture microphone 0").is_err());
    let allowed = "🎤".repeat(32);
    assert_eq!(allowed.len(), 128);
    assert_eq!(
        select_input_index(std::slice::from_ref(&allowed), &allowed).unwrap(),
        0
    );
    let oversized = "🎤".repeat(33);
    assert!(select_input_index(std::slice::from_ref(&oversized), &oversized).is_err());
}

#[test]
fn callback_handoff_preserves_every_sample_during_concurrent_consumer_drains() {
    use jarvis_voice::capture::AudioHandoff;
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicBool, Ordering},
    };
    let handoff = Arc::new(AudioHandoff::new());
    let ready = Arc::new(Barrier::new(2));
    let done = Arc::new(AtomicBool::new(false));
    let expected: Vec<f32> = (0..100000).map(|i| (i % 997) as f32 / 997.0).collect();
    let producer = handoff.clone();
    let barrier = ready.clone();
    let finished = done.clone();
    let input = expected.clone();
    let thread = std::thread::spawn(move || {
        barrier.wait();
        for chunk in input.chunks(137) {
            producer.push(chunk);
            std::thread::yield_now();
        }
        finished.store(true, Ordering::Release);
    });
    ready.wait();
    let mut actual = Vec::new();
    while !done.load(Ordering::Acquire) {
        let (samples, gap) = handoff.drain();
        assert!(!gap, "consumer contention dropped callback audio");
        assert!(samples.len() <= MAX_RECORDING_SAMPLES);
        actual.extend(samples);
        std::thread::yield_now();
    }
    thread.join().unwrap();
    let (samples, gap) = handoff.drain();
    assert!(!gap);
    actual.extend(samples);
    assert_eq!(
        actual, expected,
        "handoff reordered or lost callback samples"
    );
    assert_eq!(handoff.overruns(), 0);
    assert_eq!(handoff.drain(), (Vec::new(), false));
}

#[test]
fn stalled_consumer_only_marks_discontinuity_on_actual_capacity_exhaustion_and_recovers() {
    use jarvis_voice::capture::AudioHandoff;
    use std::sync::{Arc, mpsc};
    let handoff = Arc::new(AudioHandoff::new());
    let producer = handoff.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        producer.push(&vec![0.125; MAX_RECORDING_SAMPLES]);
        done_tx.send(()).unwrap();
    });
    done_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("producer waited for stalled consumer");
    thread.join().unwrap();
    assert_eq!(handoff.overruns(), 0);
    handoff.push(&[0.5; 320]);
    assert_eq!(handoff.overruns(), 1);
    let (samples, gap) = handoff.drain();
    assert!(gap);
    assert_eq!(samples, vec![0.125; MAX_RECORDING_SAMPLES]);
    assert_eq!(
        handoff.drain(),
        (Vec::new(), false),
        "overflow poisoned later callbacks"
    );
    handoff.push(&[0.25; 320]);
    assert_eq!(handoff.drain(), (vec![0.25; 320], false));
    assert_eq!(handoff.overruns(), 1);
}

#[cfg(target_os = "macos")]
#[test]
fn capture_diagnostics_separate_os_xruns_from_route_changes_and_queue_pressure() {
    use jarvis_voice::capture::CaptureHealth;
    let health = CaptureHealth::default();
    for _ in 0..3 {
        health.record_stream_error(cpal::ErrorKind::Xrun);
    }
    health.record_stream_error(cpal::ErrorKind::DeviceChanged);
    health.record_stream_error(cpal::ErrorKind::RealtimeDenied);
    let counters = health.diagnostics();
    assert_eq!(counters.xruns, 3);
    assert_eq!(counters.route_changes, 1);
    assert_eq!(counters.queue_overruns, 0);
    assert!(!health.failed());
    assert!(health.take_discontinuity());
    assert!(!health.take_discontinuity());
    assert_eq!(
        health.diagnostics(),
        counters,
        "consuming reset signal erased diagnostic evidence"
    );
}
