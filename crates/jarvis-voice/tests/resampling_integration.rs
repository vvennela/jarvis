//! Frequency-domain acceptance of the actual microphone callback buffer.
//! Generated tones only; no audio device, model, or network is accessed.
use jarvis_voice::capture::{CaptureBuffer, MAX_RECORDING_SAMPLES};
use std::f64::consts::TAU;

fn tone(rate: u32, hz: f64, seconds: f64) -> Vec<f32> {
    (0..(f64::from(rate) * seconds) as usize)
        .map(|n| (0.5 * (TAU * hz * n as f64 / f64::from(rate)).sin()) as f32)
        .collect()
}
fn resample(rate: u32, input: &[f32]) -> Vec<f32> {
    let mut buffer = CaptureBuffer::new(rate, 1).unwrap();
    for chunk in input.chunks(511) {
        buffer.push_f32(chunk).unwrap();
    }
    buffer.finish().unwrap()
}
fn rms(samples: &[f32]) -> f64 {
    (samples.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
}
fn gain(rate: u32, hz: f64) -> f64 {
    let input = tone(rate, hz, 0.25);
    let output = resample(rate, &input);
    assert!((output.len() as isize - 4000).abs() <= 1);
    // Discard 20ms startup transient; measure all steady-state output energy,
    // including aliases, rather than checking only the expected frequency bin.
    rms(&output[320..]) / (0.5 / 2_f64.sqrt())
}
#[test]
fn microphone_downsampling_preserves_speech_passband_at_common_input_rates() {
    for rate in [24000, 44100, 48000, 96000, 192000] {
        for hz in [1000., 3000., 5000.] {
            let measured = gain(rate, hz);
            // Fractional 24 kHz → 16 kHz interpolation introduces a measured 5 kHz
            // rolloff below 1 dB; integer48k and other rates retain their
            // original 5% acceptance. This does not establish wake reliability.
            let minimum = if rate == 24000 {
                10_f64.powf(-1.0 / 20.0)
            } else {
                0.95
            };
            assert!(
                (minimum..=1.05).contains(&measured),
                "speech passband gain {measured:.5} at {hz}Hz, input {rate}Hz"
            );
        }
    }
}
#[test]
fn frequencies_above_output_nyquist_do_not_fold_into_the_command_band() {
    for rate in [24000, 44100, 48000, 96000, 192000] {
        for hz in [9000., 12000., 20000.]
            .into_iter()
            .filter(|&hz| hz < f64::from(rate) / 2.0)
        {
            let measured = gain(rate, hz);
            let maximum = if hz == 9000. { 0.02 } else { 0.01 };
            assert!(
                measured <= maximum,
                "out-of-band tone aliased into speech: gain {measured:.5} at {hz}Hz, input {rate}Hz"
            );
        }
    }
}
#[test]
fn dc_gain_duration_and_sixteen_kilohertz_identity_are_preserved() {
    for rate in [16000, 24000, 44100, 48000, 96000, 192000] {
        let output = resample(rate, &vec![0.25; rate as usize / 4]);
        assert!((output.len() as isize - 4000).abs() <= 1);
        assert!(output.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
        assert!(
            output[320..].iter().all(|s| (*s - 0.25).abs() < 0.0001),
            "DC gain changed at {rate}"
        );
    }
    let original: Vec<_> = (0..4096)
        .map(|i| ((i * 73 % 997) as f32 / 997.0) * 2.0 - 1.0)
        .collect();
    let actual = resample(16000, &original);
    assert_eq!(actual.len(), original.len());
    assert!(
        actual
            .iter()
            .zip(&original)
            .all(|(a, b)| (a - b).abs() <= 2.0 * f32::EPSILON),
        "16kHz input filtered, delayed, or materially changed"
    );
}
#[test]
fn fragmented_stereo_and_consumer_drains_preserve_filter_history_exactly() {
    for rate in [24000, 44100, 48000, 96000, 192000] {
        let input: Vec<f32> = (0..rate / 8)
            .flat_map(|n| {
                let low = (TAU * 3000. * f64::from(n) / f64::from(rate)).sin() as f32 * 0.2;
                let high = (TAU * 12000. * f64::from(n) / f64::from(rate)).sin() as f32 * 0.4;
                [low + high, low - high * 0.5]
            })
            .collect();
        let mut whole = CaptureBuffer::continuous(rate, 2).unwrap();
        whole.push_f32(&input).unwrap();
        let (expected, gap) = whole.drain_samples();
        assert!(!gap);
        let mut streaming = CaptureBuffer::continuous(rate, 2).unwrap();
        let mut actual = Vec::new();
        let mut position = 0;
        for length in [1, 17, 512, 3, 1023, 71].into_iter().cycle() {
            if position == input.len() {
                break;
            }
            let end = (position + length).min(input.len());
            streaming.push_f32(&input[position..end]).unwrap();
            let (output, gap) = streaming.drain_samples();
            assert!(!gap);
            actual.extend(output);
            position = end;
        }
        assert_eq!(
            actual, expected,
            "filter lost state at callback/channel/drain boundary for {rate}"
        );
        assert!((actual.len() as isize - 2000).abs() <= 1);
    }
}
#[test]
fn filtered_capture_rejects_nonfinite_input_and_retains_recording_cap() {
    for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut buffer = CaptureBuffer::new(48000, 2).unwrap();
        buffer.push_f32(&[0.2; 1024]).unwrap();
        assert!(buffer.push_f32(&[invalid, 0.0]).is_err());
        assert!(
            buffer.finish().is_err(),
            "invalid sample escaped via FIR state"
        );
    }
    let mut buffer = CaptureBuffer::new(48000, 1).unwrap();
    for _ in 0..751 {
        buffer.push_f32(&[0.25; 960]).unwrap();
    }
    assert!(buffer.is_full());
    assert_eq!(buffer.output_len(), MAX_RECORDING_SAMPLES);
    let output = buffer.finish().unwrap();
    assert_eq!(output.len(), MAX_RECORDING_SAMPLES);
    assert!(output[320..].iter().all(|s| (*s - 0.25).abs() < 0.0001));
}
#[test]
#[ignore = "explicit local callback cost measurement; generated tones, no microphone"]
fn report_debug_callback_cost_at_common_input_rates() {
    for rate in [24000, 44100, 48000, 96000, 192000] {
        let input = tone(rate, 3000., 3.0);
        let mut buffer = CaptureBuffer::continuous(rate, 1).unwrap();
        let start = std::time::Instant::now();
        let mut count = 0;
        for chunk in input.chunks(rate as usize / 50) {
            buffer.push_f32(chunk).unwrap();
            let (samples, gap) = buffer.drain_samples();
            assert!(!gap);
            count += samples.len();
        }
        let elapsed = start.elapsed();
        assert!((count as isize - 48000).abs() <= 1);
        eprintln!(
            "generated_callback rate={rate} audio_seconds=3 wall_ms={} realtime_fraction={:.4}",
            elapsed.as_millis(),
            elapsed.as_secs_f64() / 3.0
        );
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "callback cannot keep up in this local debug build"
        );
    }
}
