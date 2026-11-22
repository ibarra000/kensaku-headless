//! Optional native Indra OCR engine (`accurate-ocr` feature). The pack is
//! downloaded on demand (OpenRAIL-M, never bundled); callers fall back to PP-OCR.

#[cfg(feature = "accurate-ocr")]
mod engine;
pub mod imageops;
pub mod order;
pub mod skew;
pub mod text;

use crate::config::KensakuConfig;
use crate::ocr::OcrOutcome;
use std::path::{Path, PathBuf};

/// Pack files the port reads.
pub const PACK_FILES: [&str; 8] = [
    "det.onnx",
    "rec.onnx",
    "rec_cjk.onnx",
    "rec_rtl.onnx",
    "rec_world.onnx",
    "script.onnx",
    "orient.onnx",
    "meta.json",
];

/// Pack directory; `KENSAKU_INDRA_PACK_DIR` overrides it.
pub fn pack_dir() -> PathBuf {
    match std::env::var_os("KENSAKU_INDRA_PACK_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => KensakuConfig::config_dir().join("ocr-indra"),
    }
}

pub fn pack_present() -> bool {
    // Size-checked: stale or truncated packs load but read nothing (#174).
    crate::models::indra_pack_present()
}

/// Cached self-test result; reset after a pack download (#174).
#[cfg(feature = "accurate-ocr")]
static SELF_TEST: parking_lot::Mutex<Option<Result<(), String>>> = parking_lot::Mutex::new(None);

pub fn reset_self_test() {
    #[cfg(feature = "accurate-ocr")]
    {
        *SELF_TEST.lock() = None;
    }
}

/// Why Indra cannot run, or `None` when it can.
pub fn problem() -> Option<String> {
    if !crate::ocr::accurate_compiled() {
        return Some("Indra OCR is not part of this build".into());
    }
    if !pack_present() {
        return Some("Indra OCR models are not downloaded yet".into());
    }
    if !crate::ocr::runtime_loadable() {
        return crate::ocr::runtime_error()
            .or_else(|| Some("ONNX Runtime could not be loaded".into()));
    }
    self_test().err()
}

/// Build, pack, runtime and self-test all OK.
pub fn available() -> bool {
    crate::ocr::accurate_compiled()
        && pack_present()
        && crate::ocr::runtime_loadable()
        && self_test().is_ok()
}

pub fn extract_image(path: &Path) -> OcrOutcome {
    if !(pack_present() && crate::ocr::runtime_loadable()) {
        return OcrOutcome::Unavailable;
    }
    match crate::indexer::extract::image::decode_for_recognition(path) {
        Some((image, _reserved)) => extract_decoded(&image),
        None => OcrOutcome::NoText,
    }
}

/// Reads an already-decoded image (scanned PDF pages).
pub fn extract_decoded(image: &image::DynamicImage) -> OcrOutcome {
    #[cfg(feature = "accurate-ocr")]
    {
        if !(pack_present() && crate::ocr::runtime_loadable()) {
            return OcrOutcome::Unavailable;
        }
        let page = imageops::Gray::from_image(image);
        match pool::read(&page) {
            None => OcrOutcome::Unavailable,
            Some(Ok(result)) if !result.text.trim().is_empty() => OcrOutcome::Text(result.text),
            Some(Ok(_)) => OcrOutcome::NoText,
            Some(Err(e)) => {
                eprintln!("[Kensaku OCR] Indra failed on an image: {e}");
                OcrOutcome::Unavailable
            }
        }
    }
    #[cfg(not(feature = "accurate-ocr"))]
    {
        let _ = image;
        OcrOutcome::Unavailable
    }
}

/// Reads a known image and checks the words; cached once run against a present pack.
pub fn self_test() -> Result<(), String> {
    #[cfg(feature = "accurate-ocr")]
    {
        if let Some(result) = SELF_TEST.lock().clone() {
            return result;
        }
        if !pack_present() {
            // Not cached: the pack may be downloaded later in this process.
            return Err("Indra OCR models are not downloaded yet".into());
        }
        if !crate::ocr::runtime_loadable() {
            return Err("ONNX Runtime could not be loaded".into());
        }
        let result = image::load_from_memory(crate::ocr::SELF_TEST_IMAGE)
            .map_err(|e| format!("self-test image is unreadable: {e}"))
            .and_then(|image| {
                let page = imageops::Gray::from_image(&image);
                match pool::read(&page) {
                    None => Err("Indra models failed to load".into()),
                    Some(Err(e)) => Err(format!("Indra engine failed during self-test: {e}")),
                    Some(Ok(result)) => {
                        let normalized =
                            result.text.split_whitespace().collect::<Vec<_>>().join(" ");
                        if normalized.to_lowercase().contains(crate::ocr::SELF_TEST_EXPECTED) {
                            Ok(())
                        } else if result.detected == 0 {
                            Err(
                                "Indra detected no text lines in its self-test image; \
                                 the model pack may be wrong or incomplete (re-download it)"
                                    .into(),
                            )
                        } else if result.read == 0 {
                            Err(format!(
                                "Indra detected {} lines but recognised none of them in its \
                                 self-test (recognition returned no text)",
                                result.detected
                            ))
                        } else {
                            Err(format!(
                                "Indra misread its self-test ({} of {} lines read): {normalized:?}",
                                result.read, result.detected
                            ))
                        }
                    }
                }
            });
        if let Err(reason) = &result {
            eprintln!("[Kensaku OCR] {reason}");
        }
        *SELF_TEST.lock() = Some(result.clone());
        result
    }
    #[cfg(not(feature = "accurate-ocr"))]
    {
        Err("Indra OCR is not part of this build".into())
    }
}

/// A few lazily loaded engines so workers don't queue behind one lock.
#[cfg(feature = "accurate-ocr")]
mod pool {
    use super::engine::Engine;
    use super::imageops::Gray;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, OnceLock};

    /// Threads per engine; engines = half the cores / this, at most 3.
    const THREADS: usize = 2;

    /// `None` once loading failed, so a corrupt pack is probed only once.
    static ENGINES: OnceLock<Option<Vec<Mutex<Engine>>>> = OnceLock::new();
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn load() -> Option<Vec<Mutex<Engine>>> {
        let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
        let count = (cores / 2 / THREADS).clamp(1, 4);
        // `ort` can panic instead of erroring when the runtime is broken.
        let loaded = std::panic::catch_unwind(|| {
            // `ort` allows one environment per process: share PP-OCR's.
            let env = paddleocr_rs_onnx::shared_environment().map_err(|e| e.to_string())?;
            let dir = super::pack_dir();
            (0..count)
                .map(|_| Engine::load(&env, &dir, THREADS).map(Mutex::new))
                .collect::<Result<Vec<_>, String>>()
        });
        match loaded {
            Ok(Ok(engines)) => Some(engines),
            Ok(Err(e)) => {
                eprintln!("[Kensaku OCR] Indra models failed to load: {e}");
                None
            }
            Err(_) => {
                eprintln!("[Kensaku OCR] Indra models failed to load (panic)");
                None
            }
        }
    }

    pub fn read(page: &Gray) -> Option<Result<super::engine::ReadResult, String>> {
        let engines = ENGINES.get_or_init(load).as_ref()?;
        let start = NEXT.fetch_add(1, Ordering::Relaxed);
        let mut attempts = 0;
        let mut engine = loop {
            let offset = (start + attempts) % engines.len();
            if let Some(guard) = (0..engines.len())
                .find_map(|i| engines[(offset + i) % engines.len()].try_lock().ok())
            {
                break guard;
            }
            attempts += 1;
            if attempts > 300 {
                break engines[start % engines.len()]
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| engine.read(page)));
        Some(result.unwrap_or_else(|_| Err("the engine panicked".into())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_lives_in_its_own_folder() {
        assert!(pack_dir().ends_with("ocr-indra") || std::env::var_os("KENSAKU_INDRA_PACK_DIR").is_some());
        assert!(PACK_FILES.contains(&"meta.json"));
    }

    /// PP-OCR and Indra share one `ort` environment in one process.
    #[cfg(feature = "accurate-ocr")]
    #[test]
    #[ignore = "requires KENSAKU_OCR_PACK_DIR, KENSAKU_INDRA_PACK_DIR and ORT_DYLIB_PATH"]
    fn indra_loads_after_pp_ocr_in_the_same_process() {
        assert_eq!(crate::ocr::self_test(), Ok(()), "PP-OCR self-test");
        assert_eq!(self_test(), Ok(()), "Indra self-test after PP-OCR");
    }

    /// Fixture run through the real port; needs `KENSAKU_INDRA_PACK_DIR` and `ORT_DYLIB_PATH`.
    #[cfg(feature = "accurate-ocr")]
    #[test]
    #[ignore = "requires KENSAKU_INDRA_PACK_DIR and ORT_DYLIB_PATH"]
    fn real_pack_ui_text_recall() {
        fn normalize(text: &str) -> String {
            text.chars()
                .map(|ch| if ch.is_alphanumeric() { ch } else { ' ' })
                .flat_map(|ch| ch.to_lowercase())
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        }
        assert_eq!(self_test(), Ok(()), "self-test");
        crate::ocr::set_engine(crate::ocr::INDRA);
        crate::indexer::extract::set_ocr_enabled(true);
        assert_eq!(problem(), None);
        assert_eq!(crate::ocr::usable_engine(), crate::ocr::INDRA);
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ocr-ui-text");
        let labels = std::fs::read_to_string(fixture.join("labels.txt")).expect("labels.txt");
        let labels: Vec<&str> = labels.lines().filter(|l| !l.trim().is_empty()).collect();
        let mut missed = Vec::new();
        for (index, label) in labels.iter().enumerate() {
            let path = fixture.join(format!("{:03}.png", index + 1));
            let text = match crate::indexer::extract::image::ocr_image(&path) {
                OcrOutcome::Text(text) => text,
                other => format!("{other:?}"),
            };
            if !normalize(&text).contains(&normalize(label)) {
                missed.push(format!("{label:?} -> {text:?}"));
            }
        }
        let recall = 1.0 - missed.len() as f32 / labels.len() as f32;
        println!("Indra UI-text fixture: {:.1}% recall", recall * 100.0);
        for line in missed.iter().take(10) {
            println!("  miss: {line}");
        }
        assert!(recall >= 0.95, "recall {recall:.3} below 0.95");
    }
}
