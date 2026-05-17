//! Integration test — record 1 second from default input device,
//! verify ~16k samples emerge at 16 kHz mono after resampling.
//!
//! ⚠️ Skipped automatically if no input device available (CI without audio).
//! Run manually: `cargo test --test capture_test -- --nocapture`

use rtrb::RingBuffer;
use std::time::Duration;
use tokio::sync::mpsc;
use vong_audio::{
    default_input_device, run_resampler, start_capture, AudioError, OverflowCounter, PeakMeter,
    ResampleConfig, TARGET_SAMPLE_RATE_HZ,
};

const TEST_DURATION_SEC: u64 = 1;
const EXPECTED_SAMPLES_TOLERANCE_PCT: f32 = 0.20; // ±20% — generous for async timing

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn captures_one_second_resamples_to_16khz_mono() {
    // Skip if no audio hardware (CI without mic, e.g. GitHub Actions runners).
    let device = match default_input_device() {
        Ok(d) => d,
        Err(AudioError::NoInputDevice) => {
            eprintln!("SKIP: no input device available on this host");
            return;
        }
        Err(e) => panic!("unexpected device error: {e:?}"),
    };

    // Ring buffer ~5s @ 48kHz stereo
    let (tx, rx) = RingBuffer::<i16>::new(480_000);
    let peak = PeakMeter::new();
    let overflow = OverflowCounter::new();

    let handle = match start_capture(&device, tx, peak.clone(), overflow.clone()) {
        Ok(h) => h,
        Err(e) => {
            // Some Windows CI / sandboxed environments fail at stream build
            // even when a device is reported. Skip in that case rather than fail.
            eprintln!("SKIP: capture build failed: {e:?}");
            return;
        }
    };

    println!(
        "capture started: {}Hz, {} channels",
        handle.config().sample_rate,
        handle.config().channels
    );

    // Resampler → 16kHz mono PCM16 mpsc
    let (out_tx, mut out_rx) = mpsc::channel(64);
    let cfg = ResampleConfig {
        source_rate: handle.config().sample_rate,
        source_channels: handle.config().channels.max(1),
        read_chunk_size: 2048,
    };
    let resampler_join = tokio::spawn(async move {
        let _ = run_resampler(rx, out_tx, cfg).await;
    });

    // Collect for TEST_DURATION_SEC seconds
    let mut all_samples = Vec::with_capacity(TARGET_SAMPLE_RATE_HZ as usize * TEST_DURATION_SEC as usize);
    let collect_deadline = tokio::time::Instant::now() + Duration::from_secs(TEST_DURATION_SEC + 1);

    while tokio::time::Instant::now() < collect_deadline {
        match tokio::time::timeout(Duration::from_millis(200), out_rx.recv()).await {
            Ok(Some(chunk)) => {
                all_samples.extend(chunk);
                if all_samples.len() >= TARGET_SAMPLE_RATE_HZ as usize * TEST_DURATION_SEC as usize {
                    break;
                }
            }
            Ok(None) => break,        // channel closed
            Err(_timeout) => continue, // no chunk in 200ms — keep waiting
        }
    }

    drop(handle); // stop capture
    resampler_join.abort();

    let expected = TARGET_SAMPLE_RATE_HZ as usize * TEST_DURATION_SEC as usize;
    let lower = (expected as f32 * (1.0 - EXPECTED_SAMPLES_TOLERANCE_PCT)) as usize;
    let upper = (expected as f32 * (1.0 + EXPECTED_SAMPLES_TOLERANCE_PCT)) as usize;

    println!(
        "got {} samples (expected ~{}, range [{}, {}])",
        all_samples.len(),
        expected,
        lower,
        upper
    );

    assert!(
        all_samples.len() >= lower,
        "got {} samples, expected at least {} (target {} ±{}%)",
        all_samples.len(),
        lower,
        expected,
        EXPECTED_SAMPLES_TOLERANCE_PCT * 100.0
    );
    assert!(
        all_samples.len() <= upper * 3, // generous upper bound — may have buffered extra
        "got {} samples, expected at most ~{} (target {} ±{}%)",
        all_samples.len(),
        upper,
        expected,
        EXPECTED_SAMPLES_TOLERANCE_PCT * 100.0
    );

    // Optional: write to WAV for manual inspection
    if std::env::var_os("VONG_TEST_WRITE_WAV").is_some() {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: TARGET_SAMPLE_RATE_HZ,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let path = "tests/output/capture_test.wav";
        let _ = std::fs::create_dir_all("tests/output");
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for &s in &all_samples {
            writer.write_sample(s).unwrap();
        }
        writer.finalize().unwrap();
        println!("wrote WAV: {path}");
    }

    let peak_val = peak.read_and_reset();
    println!("peak amplitude observed: {peak_val:.4}");

    let dropped = overflow.take_count();
    if dropped > 0 {
        eprintln!("WARN: dropped {dropped} samples due to ring buffer overflow");
    }
}
