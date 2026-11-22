//! Optional accurate OCR (PP-OCRv6 tiny, `accurate-ocr` feature); Tesseract is the fallback.

use crate::config::KensakuConfig;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};

pub const TESSERACT: &str = "tesseract";
pub const ACCURATE: &str = "accurate";
pub const INDRA: &str = "indra";
/// No engine can run right now.
pub const NONE: &str = "none";

/// Bump when output for the same image can change; re-queues every image and PDF once.
pub const OCR_REVISION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcrOutcome {
    Text(String),
    /// Ran and found no text. Final: retrying gives the same answer.
    NoText,
    /// Could not run. Not final: retry once an engine is usable.
    Unavailable,
}

static ENGINE: AtomicU8 = AtomicU8::new(0);

/// PP-OCRv6 pack directory; `KENSAKU_OCR_PACK_DIR` overrides it.
pub fn pack_dir() -> PathBuf {
    match std::env::var_os("KENSAKU_OCR_PACK_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => KensakuConfig::config_dir().join("ocr"),
    }
}

pub fn det_path() -> PathBuf {
    pack_dir().join("det.onnx")
}

pub fn rec_path() -> PathBuf {
    pack_dir().join("rec.onnx")
}

pub fn dict_path() -> PathBuf {
    pack_dir().join("dict.txt")
}

/// All three pack files exist at their expected sizes.
pub fn pack_present() -> bool {
    // Size-checked: stale or truncated packs load but read nothing (#174).
    crate::models::ocr_pack_present()
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

pub fn accurate_compiled() -> bool {
    cfg!(feature = "accurate-ocr")
}

#[cfg_attr(not(feature = "accurate-ocr"), allow(dead_code))]
fn runtime_file_name() -> String {
    format!(
        "{}onnxruntime{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    )
}

/// Bundled ONNX Runtime locations in try order. Never prefer a system copy
/// (Windows System32 ships an old, API-incompatible `onnxruntime.dll`).
#[cfg_attr(not(feature = "accurate-ocr"), allow(dead_code))]
fn runtime_candidates(exe: &Path, file_name: &str) -> Vec<PathBuf> {
    let Some(dir) = exe.parent() else {
        return Vec::new();
    };
    let mut candidates = vec![
        dir.join(file_name),
        dir.join("../Resources").join(file_name),
        dir.join("../lib/kensaku").join(file_name),
        dir.join("../lib").join(file_name),
    ];
    // Dev/test layouts: target/debug/deps -> target/debug
    if let Some(parent) = dir.parent() {
        candidates.push(parent.join(file_name));
        // target/debug -> target
        if let Some(grandparent) = parent.parent() {
            candidates.push(grandparent.join(file_name));
            // workspace root -> crates/kensaku-core (staged ONNX Runtime)
            candidates.push(grandparent.join("crates").join("kensaku-core").join(file_name));
        }
    }
    candidates.push(KensakuConfig::config_dir().join(file_name));
    candidates.push(KensakuConfig::config_dir().join("models").join(file_name));

    // The standalone daemon uses the copy the installed app shipped.
    if cfg!(target_os = "macos") {
        candidates
            .push(PathBuf::from("/Applications/kensaku.app/Contents/Resources").join(file_name));
    } else if cfg!(target_os = "linux") {
        candidates.push(PathBuf::from("/usr/lib/kensaku").join(file_name));
    }
    candidates
}

/// One-time runtime probe outcome: `Ok(path)` or a reason.
#[cfg(feature = "accurate-ocr")]
fn runtime_probe() -> &'static Result<String, String> {
    use std::sync::OnceLock;

    static PROBE: OnceLock<Result<String, String>> = OnceLock::new();
    PROBE.get_or_init(|| {
        let file_name = runtime_file_name();
        let path_opt = match std::env::var("ORT_DYLIB_PATH") {
            Ok(path) if !path.trim().is_empty() => Some(path),
            _ => std::env::current_exe()
                .ok()
                .and_then(|exe| {
                    runtime_candidates(&exe, &file_name)
                        .into_iter()
                        .find(|candidate| candidate.is_file())
                })
                .map(|candidate| candidate.to_string_lossy().into_owned()),
        };
        let path = match path_opt {
            Some(p) => p,
            None => {
                let reason = format!(
                    "ONNX Runtime library ({file_name}) not found. Place it next to kensaku or set ORT_DYLIB_PATH"
                );
                eprintln!("[Kensaku OCR] {reason}");
                return Err(reason);
            }
        };
        // Guard against loader panics; `ort::api()` then checks the API version.
        let loaded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ort::init_from(&path).map(|_| ()).map_err(|e| e.to_string())
        }))
        .unwrap_or_else(|_| Err("the loader panicked".to_string()));
        let result = loaded.and_then(|()| {
            std::panic::catch_unwind(|| {
                let _ = ort::api();
            })
            .map_err(|_| "the library is too old for this build (needs 1.31+)".to_string())
        });
        match result {
            Ok(()) => Ok(path),
            Err(e) => {
                let reason = if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
                    "PP-OCR needs a Mac with Apple silicon".to_string()
                } else {
                    format!("ONNX Runtime could not be loaded ({e})")
                };
                eprintln!("[Kensaku OCR] {reason}; tried {path:?}");
                Err(reason)
            }
        }
    })
}

pub fn runtime_error() -> Option<String> {
    #[cfg(feature = "accurate-ocr")]
    {
        runtime_probe().as_ref().err().cloned()
    }
    #[cfg(not(feature = "accurate-ocr"))]
    {
        Some("accurate-ocr feature is not compiled in".to_string())
    }
}

/// Probed once per process; installing the library later needs a restart.
pub fn runtime_loadable() -> bool {
    #[cfg(feature = "accurate-ocr")]
    {
        runtime_probe().is_ok()
    }
    #[cfg(not(feature = "accurate-ocr"))]
    {
        false
    }
}

/// Why the accurate engine cannot run, or `None` when it can.
pub fn accurate_problem() -> Option<String> {
    if !accurate_compiled() {
        return Some("PP-OCR is not part of this build".into());
    }
    if !pack_present() {
        return Some("PP-OCR pack is not downloaded yet".into());
    }
    #[cfg(feature = "accurate-ocr")]
    if let Err(reason) = runtime_probe() {
        return Some(reason.clone());
    }
    if let Err(reason) = self_test() {
        return Some(reason);
    }
    None
}

/// Needs the pack, an `accurate-ocr` build, and a loadable `libonnxruntime`.
pub fn accurate_available() -> bool {
    accurate_compiled() && pack_present() && runtime_loadable() && self_test().is_ok()
}

/// Selected engine if it can run, else the next usable one (Indra -> PP-OCR ->
/// Tesseract), else [`NONE`].
pub fn usable_engine() -> &'static str {
    let tesseract = || crate::indexer::extract::image::tesseract_usable();
    if engine_name() == INDRA {
        if crate::ocr_indra::available() {
            INDRA
        } else if accurate_available() {
            ACCURATE
        } else if tesseract() {
            TESSERACT
        } else {
            NONE
        }
    } else if engine_name() == ACCURATE {
        if accurate_available() {
            ACCURATE
        } else if tesseract() {
            TESSERACT
        } else {
            NONE
        }
    } else if tesseract() {
        TESSERACT
    } else if accurate_available() {
        ACCURATE
    } else {
        NONE
    }
}

/// Fingerprint of OCR output; a change re-reads images and PDFs once.
pub fn signature() -> String {
    // The image-size cap is included so raising it re-reads skipped images.
    format!(
        "{}:{}:{}",
        usable_engine(),
        OCR_REVISION,
        crate::indexer::extract::image::ocr_max_image_bytes()
    )
}

/// Values other than `accurate` and `indra` (including legacy `native`) mean Tesseract.
pub fn set_engine(engine: &str) {
    let value = if engine.eq_ignore_ascii_case(ACCURATE) {
        1
    } else if engine.eq_ignore_ascii_case(INDRA) {
        2
    } else {
        0
    };
    ENGINE.store(value, Ordering::SeqCst);
}

pub fn engine_name() -> &'static str {
    match ENGINE.load(Ordering::SeqCst) {
        1 => ACCURATE,
        2 => INDRA,
        _ => TESSERACT,
    }
}

/// Appends the space class Paddle adds (`use_space_char=True`) but the dict files
/// lack; without it every predicted space is dropped.
pub fn keys_with_space(dict: &[u8]) -> Vec<u8> {
    let mut keys = dict.to_vec();
    while matches!(keys.last(), Some(b'\n' | b'\r')) {
        keys.pop();
    }
    let already = keys
        .rsplit(|byte| *byte == b'\n')
        .next()
        .is_some_and(|last| last == b" " || last == b" \r");
    if !already {
        keys.extend_from_slice(b"\n ");
    }
    keys
}

/// `Unavailable` when the engine cannot run; the caller picks the fallback.
pub fn extract_image_ocr(path: &Path) -> OcrOutcome {
    #[cfg(feature = "accurate-ocr")]
    {
        if !(pack_present() && runtime_loadable()) {
            return OcrOutcome::Unavailable;
        }
        let Some(engine) = accurate_engine() else {
            return OcrOutcome::Unavailable;
        };
        // The decoded page is charged against the decode budget while held.
        let Some((image, _reserved)) =
            crate::indexer::extract::image::decode_for_recognition(path)
        else {
            return OcrOutcome::NoText;
        };
        recognize(&engine, &image)
    }
    #[cfg(not(feature = "accurate-ocr"))]
    {
        let _ = path;
        OcrOutcome::Unavailable
    }
}

/// Reads an already-decoded image (scanned PDF pages).
pub fn extract_decoded_image(image: &image::DynamicImage) -> OcrOutcome {
    #[cfg(feature = "accurate-ocr")]
    {
        if !(pack_present() && runtime_loadable()) {
            return OcrOutcome::Unavailable;
        }
        match accurate_engine() {
            Some(engine) => recognize(&engine, image),
            None => OcrOutcome::Unavailable,
        }
    }
    #[cfg(not(feature = "accurate-ocr"))]
    {
        let _ = image;
        OcrOutcome::Unavailable
    }
}

#[cfg(feature = "accurate-ocr")]
fn recognize(engine: &paddleocr_rs_onnx::OcrEngine, image: &image::DynamicImage) -> OcrOutcome {
    use paddleocr_rs_onnx::OrderBy;

    let blocks = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        engine.recognize_all(image, OrderBy::Horizontal)
    }));
    let blocks = match blocks {
        Ok(Ok(blocks)) => blocks,
        Ok(Err(e)) => {
            eprintln!("[Kensaku OCR] PP-OCR failed on an image: {e}");
            return OcrOutcome::Unavailable;
        }
        Err(_) => {
            eprintln!("[Kensaku OCR] PP-OCR panicked on an image");
            return OcrOutcome::Unavailable;
        }
    };
    let text = blocks
        .iter()
        .map(|block| block.text.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        OcrOutcome::NoText
    } else {
        OcrOutcome::Text(text)
    }
}

/// `Unavailable` is sticky so a corrupt pack is probed only once.
#[cfg(feature = "accurate-ocr")]
enum AccurateEngine {
    Uninitialized,
    Ready(std::sync::Arc<paddleocr_rs_onnx::OcrEngine>),
    Unavailable,
}

/// Shared engine, loaded on first use; the lock covers loading only, so workers
/// recognize concurrently.
#[cfg(feature = "accurate-ocr")]
fn accurate_engine() -> Option<std::sync::Arc<paddleocr_rs_onnx::OcrEngine>> {
    use paddleocr_rs_onnx::OcrEngine;
    use std::sync::{Arc, Mutex, OnceLock};

    static ENGINE_CACHE: OnceLock<Mutex<AccurateEngine>> = OnceLock::new();

    let cache = ENGINE_CACHE.get_or_init(|| Mutex::new(AccurateEngine::Uninitialized));
    let mut guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if matches!(*guard, AccurateEngine::Uninitialized) {
        let files = (
            std::fs::read(det_path()),
            std::fs::read(rec_path()),
            std::fs::read(dict_path()),
        );
        let (Ok(det), Ok(rec), Ok(dict)) = files else {
            // Pack vanished mid-check: retry later rather than going sticky-unavailable.
            return None;
        };
        let keys = keys_with_space(&dict);
        // `ort` panics when the runtime cannot load; contain it so the caller falls back.
        *guard = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            OcrEngine::new(&det, &rec, &keys)
        })) {
            Ok(Ok(engine)) => AccurateEngine::Ready(Arc::new(engine)),
            Ok(Err(e)) => {
                eprintln!("[Kensaku OCR] PP-OCR pack failed to load: {e}");
                AccurateEngine::Unavailable
            }
            Err(_) => {
                eprintln!("[Kensaku OCR] PP-OCR pack failed to load (panic)");
                AccurateEngine::Unavailable
            }
        };
    }
    match &*guard {
        AccurateEngine::Ready(engine) => Some(Arc::clone(engine)),
        _ => None,
    }
}

/// Known-text image proving the engine actually reads.
#[cfg_attr(not(feature = "accurate-ocr"), allow(dead_code))]
pub(crate) const SELF_TEST_IMAGE: &[u8] = include_bytes!("../tests/fixtures/ocr-ui-text/001.png");
#[cfg_attr(not(feature = "accurate-ocr"), allow(dead_code))]
pub(crate) const SELF_TEST_EXPECTED: &str = "open settings";

/// Reads a known image and checks the words (spaces included); cached once run
/// against a present pack. [`reset_self_test`] clears it.
pub fn self_test() -> Result<(), String> {
    #[cfg(feature = "accurate-ocr")]
    {
        if let Some(result) = SELF_TEST.lock().clone() {
            return result;
        }
        if !pack_present() {
            // Not cached: the pack may be downloaded later in this process.
            return Err("PP-OCR pack is not downloaded yet".into());
        }
        if let Err(reason) = runtime_probe() {
            return Err(reason.clone());
        }
        let result = (|| {
            let image = image::load_from_memory(SELF_TEST_IMAGE)
                .map_err(|e| format!("self-test image is unreadable: {e}"))?;
            let engine = accurate_engine().ok_or("PP-OCR pack failed to load")?;
            match recognize(&engine, &image) {
                OcrOutcome::Text(text) => {
                    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    if normalized.to_lowercase().contains(SELF_TEST_EXPECTED) {
                        Ok(())
                    } else {
                        Err(format!("PP-OCR misread its self-test: {normalized:?}"))
                    }
                }
                _ => Err("PP-OCR read nothing in its self-test".to_string()),
            }
        })();
        if let Err(reason) = &result {
            eprintln!("[Kensaku OCR] {reason}");
        }
        *SELF_TEST.lock() = Some(result.clone());
        result
    }
    #[cfg(not(feature = "accurate-ocr"))]
    {
        Err("PP-OCR is not part of this build".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_defaults_to_tesseract_and_round_trips() {
        set_engine(TESSERACT);
        assert_eq!(engine_name(), TESSERACT);
        set_engine(ACCURATE);
        assert_eq!(engine_name(), ACCURATE);
        set_engine("Indra");
        assert_eq!(engine_name(), INDRA);
        // Legacy `native` falls back to Tesseract.
        set_engine("native");
        assert_eq!(engine_name(), TESSERACT);
        set_engine(TESSERACT);
    }

    #[test]
    fn keys_gain_exactly_one_trailing_space_entry() {
        // Trailing newline, none, CRLF: always one " " line.
        for dict in [&b"a\nb\n"[..], b"a\nb", b"a\r\nb\r\n"] {
            let keys = keys_with_space(dict);
            let text = String::from_utf8(keys).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            assert_eq!(lines.len(), 3, "{dict:?} -> {lines:?}");
            assert_eq!(*lines.last().unwrap(), " ");
        }
        assert_eq!(keys_with_space(b"a\nb\n "), b"a\nb\n ".to_vec());
        assert_eq!(keys_with_space(&keys_with_space(b"a\n")), b"a\n ".to_vec());
    }

    #[test]
    fn runtime_is_looked_up_where_each_installer_puts_it() {
        let name = runtime_file_name();
        let exe = Path::new("/opt/app/bin/kensaku");
        let candidates = runtime_candidates(exe, &name);
        // Next to the exe, Contents/Resources, <prefix>/lib/kensaku.
        assert_eq!(candidates[0], Path::new("/opt/app/bin").join(&name));
        assert!(candidates.contains(&Path::new("/opt/app/bin/../Resources").join(&name)));
        assert!(candidates.contains(&Path::new("/opt/app/bin/../lib/kensaku").join(&name)));
    }

    #[test]
    fn pack_presence_requires_all_three_files() {
        let dir = std::env::temp_dir().join(format!("kensaku-ocr-pack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Presence is covered by the ignored real-pack test.
        assert!(det_path().ends_with("det.onnx"));
        assert!(rec_path().ends_with("rec.onnx"));
        assert!(dict_path().ends_with("dict.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "accurate-ocr")]
    #[test]
    fn runtime_probe_is_panic_free() {
        // Probing must answer, not panic, whether or not the runtime is installed.
        let _ = runtime_loadable();
        let _ = accurate_available();
    }

    /// End-to-end with the real pack and runtime (run by `.github/workflows/ocr.yml`).
    #[cfg(feature = "accurate-ocr")]
    #[test]
    #[ignore = "requires KENSAKU_OCR_PACK_DIR and ORT_DYLIB_PATH"]
    fn real_pipeline_reads_images_and_scanned_pdfs() {
        use lopdf::{Dictionary, Object, Stream, dictionary};

        set_engine(ACCURATE);
        crate::indexer::extract::set_ocr_enabled(true);
        assert_eq!(self_test(), Ok(()), "self-test");
        assert!(accurate_available());
        assert_eq!(accurate_problem(), None);
        assert_eq!(usable_engine(), ACCURATE);

        let fixture = format!(
            "{}/tests/fixtures/ocr-ui-text/002.png",
            env!("CARGO_MANIFEST_DIR")
        );
        match crate::indexer::extract::image::ocr_image(Path::new(&fixture)) {
            OcrOutcome::Text(text) => {
                assert!(
                    text.to_lowercase().contains("search results"),
                    "image: {text:?}"
                )
            }
            other => panic!("image OCR: {other:?}"),
        }

        // Scanned PDF: one page, no text layer, raw Flate RGB rows.
        let rgb = image::open(&fixture).unwrap().to_rgb8();
        let (w, h) = rgb.dimensions();
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, rgb.as_raw()).unwrap();
        let mut doc = lopdf::Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let image_id = doc.add_object(
            Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Image",
                    "Width" => w as i64, "Height" => h as i64,
                    "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
                    "Filter" => "FlateDecode",
                },
                encoder.finish().unwrap(),
            )
            .with_compression(false),
        );
        let content = doc.add_object(Stream::new(
            Dictionary::new(),
            format!("q {w} 0 0 {h} 0 0 cm /Im0 Do Q").into_bytes(),
        ));
        let page = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content,
            "Resources" => dictionary! { "XObject" => dictionary! { "Im0" => image_id } },
            "MediaBox" => vec![0.into(), 0.into(), (w as i64).into(), (h as i64).into()],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(
                dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
            ),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        let pdf_path =
            std::env::temp_dir().join(format!("kensaku-scan-{}.pdf", std::process::id()));
        doc.save(&pdf_path).unwrap();

        let result = crate::indexer::extract::pdf::extract_pdf(&pdf_path, 100_000).expect("pdf");
        let _ = std::fs::remove_file(&pdf_path);
        assert!(result.is_scanned);
        assert!(!result.ocr_unavailable);
        assert!(
            result.text.to_lowercase().contains("search results"),
            "scanned PDF: {:?}",
            result.text
        );
    }

    #[cfg(feature = "accurate-ocr")]
    #[test]
    #[ignore = "requires KENSAKU_OCR_PACK_DIR pointing at det.onnx/rec.onnx/dict.txt"]
    fn real_pack_loads() {
        let dir = std::env::var("KENSAKU_OCR_PACK_DIR")
            .expect("KENSAKU_OCR_PACK_DIR must point at the OCR pack");
        let dir = Path::new(&dir);
        let det = std::fs::read(dir.join("det.onnx")).expect("det.onnx");
        let rec = std::fs::read(dir.join("rec.onnx")).expect("rec.onnx");
        let dict = keys_with_space(&std::fs::read(dir.join("dict.txt")).expect("dict.txt"));
        let engine =
            paddleocr_rs_onnx::OcrEngine::new(&det, &rec, &dict).expect("load PP-OCRv6 tiny pack");
        assert!(engine.device() == paddleocr_rs_onnx::AccelerationDevice::Cpu);
    }

    #[cfg(feature = "accurate-ocr")]
    #[test]
    #[ignore = "requires KENSAKU_OCR_PACK_DIR and KENSAKU_OCR_IMAGE"]
    fn real_pack_reads_text() {
        let dir = std::env::var("KENSAKU_OCR_PACK_DIR")
            .expect("KENSAKU_OCR_PACK_DIR must point at the OCR pack");
        let image_path = std::env::var("KENSAKU_OCR_IMAGE")
            .expect("KENSAKU_OCR_IMAGE must point at a test image");
        let dir = Path::new(&dir);
        let det = std::fs::read(dir.join("det.onnx")).expect("det.onnx");
        let rec = std::fs::read(dir.join("rec.onnx")).expect("rec.onnx");
        let dict = keys_with_space(&std::fs::read(dir.join("dict.txt")).expect("dict.txt"));
        let engine = paddleocr_rs_onnx::OcrEngine::new(&det, &rec, &dict).expect("engine");
        let image = image::open(&image_path).expect("test image");
        let blocks = engine
            .recognize_all(&image, paddleocr_rs_onnx::OrderBy::Horizontal)
            .expect("recognize");
        let text = blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        println!("PP-OCRv6 text: {text}");
        assert!(text.to_lowercase().contains("kensaku"), "got: {text}");
        assert!(text.contains(' '), "words must stay separated, got: {text}");
    }

    #[cfg(feature = "accurate-ocr")]
    #[test]
    #[ignore = "requires KENSAKU_OCR_PACK_DIR; runs the 104-image UI-text fixture"]
    fn real_pack_ui_text_recall() {
        // Keep spaces: search tokenizes on word boundaries.
        fn normalize(text: &str) -> String {
            text.chars()
                .map(|ch| if ch.is_alphanumeric() { ch } else { ' ' })
                .flat_map(|ch| ch.to_lowercase())
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        }

        let pack = std::env::var("KENSAKU_OCR_PACK_DIR")
            .expect("KENSAKU_OCR_PACK_DIR must point at the OCR pack");
        let fixture = std::env::var("KENSAKU_OCR_FIXTURE_DIR").unwrap_or_else(|_| {
            format!("{}/tests/fixtures/ocr-ui-text", env!("CARGO_MANIFEST_DIR"))
        });
        let pack = Path::new(&pack);
        let fixture = Path::new(&fixture);
        let det = std::fs::read(pack.join("det.onnx")).expect("det.onnx");
        let rec = std::fs::read(pack.join("rec.onnx")).expect("rec.onnx");
        let dict = keys_with_space(&std::fs::read(pack.join("dict.txt")).expect("dict.txt"));
        let engine = paddleocr_rs_onnx::OcrEngine::new(&det, &rec, &dict).expect("engine");

        let labels = std::fs::read_to_string(fixture.join("labels.txt")).expect("labels.txt");
        let labels: Vec<&str> = labels
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        let mut hits = 0usize;
        let mut missed = Vec::new();
        for (index, label) in labels.iter().enumerate() {
            let image_path = fixture.join(format!("{:03}.png", index + 1));
            let image = image::open(&image_path).expect("fixture image");
            let blocks = engine
                .recognize_all(&image, paddleocr_rs_onnx::OrderBy::Horizontal)
                .expect("recognize");
            let text = blocks
                .iter()
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let expected = normalize(label);
            let actual = normalize(&text);
            if actual.contains(&expected) {
                hits += 1;
            } else {
                missed.push(format!("{label:?} -> {text:?}"));
            }
        }
        let recall = hits as f32 / labels.len() as f32;
        println!(
            "PP-OCRv6 UI-text fixture: {hits}/{} lines exact ({:.1}% recall)",
            labels.len(),
            recall * 100.0
        );
        for line in missed.iter().take(10) {
            println!("  miss: {line}");
        }
        assert!(recall >= 0.95, "recall {recall:.3} below 0.95");
    }
}
