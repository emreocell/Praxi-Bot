//! Optical Character Recognition (OCR) trait and engine implementations (`ocrs` pure-Rust
//! engine and optional `Windows.Media.Ocr` native engine behind the `windows-ocr` feature).

use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use image::RgbImage;
use ocrs::{ImageSource, OcrEngine as InnerOcrsEngine, OcrEngineParams, TextItem};
use rten::Model;
use sha2::{Digest, Sha256};

use crate::{BoundingBox, VisionError};

/// Upstream URL for the default `ocrs` text detection model (Apache-2.0 / MIT).
pub const DETECTION_MODEL_URL: &str =
    "https://ocrs-models.s3-accelerate.amazonaws.com/text-detection.onnx";

/// Upstream URL for the default `ocrs` text recognition model (Apache-2.0 / MIT).
pub const RECOGNITION_MODEL_URL: &str =
    "https://ocrs-models.s3-accelerate.amazonaws.com/text-recognition.onnx";

/// Default filename for the detection model in the local model directory.
pub const DETECTION_MODEL_FILENAME: &str = "text-detection.onnx";

/// Default filename for the recognition model in the local model directory.
pub const RECOGNITION_MODEL_FILENAME: &str = "text-recognition.onnx";

/// Largest model file that is read, in bytes. The real models are about 2.4 MB (detection) and
/// 9.3 MB (recognition); the cap only stops a planted file from exhausting memory.
pub const MAX_MODEL_BYTES: u64 = 64 * 1024 * 1024;

/// SHA-256 digests (lowercase hex) the detection model is accepted under.
///
/// Taken on 2026-11-27 from one download of [`DETECTION_MODEL_URL`] over HTTPS (2499479
/// bytes), hashed with `certutil -hashfile` and `Get-FileHash` (both agree). The file was only
/// hashed, never loaded. Update together with the URL when upstream publishes a new model. An
/// empty list refuses every file.
pub const DETECTION_MODEL_SHA256: &[&str] =
    &["a917b23dbd9524b465df7e922641b3ff2981623df4ded5a0234004ef2fee7cfe"];

/// SHA-256 digests (lowercase hex) the recognition model is accepted under.
///
/// Taken on 2026-11-27 from one download of [`RECOGNITION_MODEL_URL`] over HTTPS (9713177
/// bytes), hashed with `certutil -hashfile` and `Get-FileHash` (both agree). The file was only
/// hashed, never loaded. Update together with the URL when upstream publishes a new model. An
/// empty list refuses every file.
pub const RECOGNITION_MODEL_SHA256: &[&str] =
    &["86c145c2edb96c8caed5b1ebb8f44d706408922211451c309c625157dd6061c5"];

/// How many trailing components of a model directory are checked for links. That is the part
/// of the default layout (`praxi-bot/models/ocrs`) that Praxi owns; the components above it
/// are system locations, which may legitimately be links (`/var` on macOS).
const LINK_CHECKED_DIR_LEVELS: usize = 3;

/// SHA-256 digests (lowercase hex) each model file must match before it is loaded.
///
/// An empty list refuses the file, so a build without pinned digests loads nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelDigests {
    /// Digests the detection model is accepted under.
    pub detection: Vec<String>,
    /// Digests the recognition model is accepted under.
    pub recognition: Vec<String>,
}

impl ModelDigests {
    /// The digests pinned in this build ([`DETECTION_MODEL_SHA256`] and
    /// [`RECOGNITION_MODEL_SHA256`]).
    #[must_use]
    pub fn pinned() -> Self {
        let owned = |digests: &[&str]| digests.iter().map(ToString::to_string).collect();
        Self {
            detection: owned(DETECTION_MODEL_SHA256),
            recognition: owned(RECOGNITION_MODEL_SHA256),
        }
    }
}

/// Granularity of OCR output boxes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OcrGranularity {
    /// Emit one [`TextBox`] per recognized text line (default).
    #[default]
    Line,
    /// Emit one [`TextBox`] per individual word.
    Word,
}

/// A recognized text region on screen with its bounding box and confidence score.
#[derive(Debug, Clone, PartialEq)]
pub struct TextBox {
    /// Recognized UTF-8 text content.
    pub text: String,
    /// Pixel bounding box of the recognized text in image coordinates.
    pub bbox: BoundingBox,
    /// Confidence score in `[0.0, 1.0]`.
    pub confidence: f32,
}

impl TextBox {
    /// Creates a new [`TextBox`].
    #[must_use]
    pub const fn new(text: String, bbox: BoundingBox, confidence: f32) -> Self {
        Self {
            text,
            bbox,
            confidence,
        }
    }
}

/// Trait implemented by OCR engines used when the accessibility tree is weak or absent.
pub trait OcrEngine {
    /// Recognizes text regions in `img` and returns their bounding boxes in reading order.
    fn recognize(&self, img: &RgbImage) -> Vec<TextBox>;
}

/// File paths for the `ocrs` detection and recognition neural network models.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcrModelPaths {
    /// Path to the text detection model (`.onnx` or `.rten`).
    pub detection_model: PathBuf,
    /// Path to the text recognition model (`.onnx` or `.rten`).
    pub recognition_model: PathBuf,
}

impl OcrModelPaths {
    /// Constructs model paths inside `dir` using the default filenames.
    #[must_use]
    pub fn from_dir(dir: impl AsRef<Path>) -> Self {
        let dir = dir.as_ref();
        Self {
            detection_model: dir.join(DETECTION_MODEL_FILENAME),
            recognition_model: dir.join(RECOGNITION_MODEL_FILENAME),
        }
    }

    /// Returns the default local data directory for `ocrs` models, always an absolute path.
    ///
    /// Resolution order:
    /// 1. `PRAXI_OCR_MODEL_DIR` environment variable when it is an absolute path (a relative
    ///    value is ignored with a warning).
    /// 2. Platform data dir (`%LOCALAPPDATA%\praxi-bot\models\ocrs` on Windows,
    ///    `$XDG_DATA_HOME/praxi-bot/models/ocrs` or `~/.local/share/praxi-bot/models/ocrs` on
    ///    Unix); a base that is not absolute is skipped.
    ///
    /// The user's cache folder (`~/.cache/ocrs`) is deliberately not consulted, and there is no
    /// relative fallback: with no absolute base this returns `None`.
    #[must_use]
    pub fn default_data_dir() -> Option<PathBuf> {
        resolve_data_dir(
            env::var_os("PRAXI_OCR_MODEL_DIR").as_deref(),
            env::var_os("LOCALAPPDATA").as_deref(),
            env::var_os("XDG_DATA_HOME").as_deref(),
            home_dir().as_deref(),
        )
    }

    /// The model directory of a daemon: `PRAXI_OCR_MODEL_DIR` when it is an absolute path,
    /// otherwise `models/ocrs` inside the daemon's own `data_dir`. That folder is the one the
    /// file tools refuse to write, so a bot cannot plant model files there. `None` when
    /// `data_dir` is not absolute (never a relative fallback).
    #[must_use]
    pub fn model_dir_in(data_dir: &Path) -> Option<PathBuf> {
        resolve_model_dir(env::var_os("PRAXI_OCR_MODEL_DIR").as_deref(), data_dir)
    }

    /// Returns `true` if both model files exist on disk. This does not vet them; see
    /// [`OcrModelPaths::verify`].
    #[must_use]
    pub fn exists(&self) -> bool {
        self.detection_model.is_file() && self.recognition_model.is_file()
    }

    /// Vets both model files against the digests pinned in this build, without loading them.
    ///
    /// # Errors
    ///
    /// Returns [`VisionError::Ocr`] naming the first file that is refused and why (see
    /// [`OcrModelPaths::verify_with`]).
    pub fn verify(&self) -> Result<(), VisionError> {
        self.verify_with(&ModelDigests::pinned())
    }

    /// Vets both model files against `digests`, without loading them.
    ///
    /// Each file must be an absolute path to a regular file that is not a symbolic link or
    /// reparse point (nor behind one, in the Praxi-owned part of its directory path), at most
    /// [`MAX_MODEL_BYTES`] long, and must hash to one of the accepted SHA-256 digests.
    ///
    /// # Errors
    ///
    /// Returns [`VisionError::Ocr`] naming the first file that is refused and why.
    pub fn verify_with(&self, digests: &ModelDigests) -> Result<(), VisionError> {
        read_verified_model(&self.detection_model, &digests.detection, MAX_MODEL_BYTES)?;
        read_verified_model(
            &self.recognition_model,
            &digests.recognition,
            MAX_MODEL_BYTES,
        )?;
        Ok(())
    }
}

fn home_dir() -> Option<OsString> {
    env::var_os("USERPROFILE")
        .filter(|v| !v.is_empty())
        .or_else(|| env::var_os("HOME"))
}

/// Pure resolver behind [`OcrModelPaths::default_data_dir`] (testable without touching the
/// process environment). Never returns a relative path.
fn resolve_data_dir(
    model_dir_override: Option<&OsStr>,
    local_app_data: Option<&OsStr>,
    xdg_data_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    if let Some(custom) = model_dir_override.filter(|v| !v.to_string_lossy().trim().is_empty()) {
        let custom = PathBuf::from(custom);
        if custom.is_absolute() {
            return Some(custom);
        }
        tracing::warn!("PRAXI_OCR_MODEL_DIR is ignored: it must be an absolute path");
    }

    let praxi_models = |base: PathBuf| base.join("praxi-bot").join("models").join("ocrs");
    for base in [local_app_data, xdg_data_home]
        .into_iter()
        .flatten()
        .map(PathBuf::from)
    {
        if base.is_absolute() {
            return Some(praxi_models(base));
        }
    }
    home.map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .map(|home| praxi_models(home.join(".local").join("share")))
}

/// Pure resolver behind [`OcrModelPaths::model_dir_in`].
fn resolve_model_dir(model_dir_override: Option<&OsStr>, data_dir: &Path) -> Option<PathBuf> {
    if let Some(custom) = model_dir_override.filter(|v| !v.to_string_lossy().trim().is_empty()) {
        let custom = PathBuf::from(custom);
        if custom.is_absolute() {
            return Some(custom);
        }
        tracing::warn!("PRAXI_OCR_MODEL_DIR is ignored: it must be an absolute path");
    }
    data_dir
        .is_absolute()
        .then(|| data_dir.join("models").join("ocrs"))
}

fn refuse(path: &Path, why: &str) -> VisionError {
    VisionError::Ocr(format!("model file {} refused: {why}", path.display()))
}

fn is_link_or_reparse(meta: &fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// Reads at most `max_bytes` from `reader`; `None` when it holds more. Never reads more than
/// `max_bytes + 1` bytes, so an endless or oversized source cannot exhaust memory.
fn read_capped(reader: impl Read, max_bytes: u64, size_hint: u64) -> io::Result<Option<Vec<u8>>> {
    let capacity = usize::try_from(size_hint.min(max_bytes)).unwrap_or_default();
    let mut bytes = Vec::with_capacity(capacity);
    reader
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    Ok(u64::try_from(bytes.len())
        .is_ok_and(|len| len <= max_bytes)
        .then_some(bytes))
}

/// Vets one model file and returns its bytes, read once, so the caller loads exactly the
/// bytes that were hashed (no check-then-use gap on the path).
///
/// The checks, in order: a pinned digest exists (an empty `accepted` fails closed); the path
/// is absolute; the last [`LINK_CHECKED_DIR_LEVELS`] directory components, resolved under the
/// canonical form of their parent, are real directories (no symbolic link, junction or other
/// reparse point); the file is a regular file and not a link; its size is within `max_bytes`
/// (checked before it is opened, and again on the opened handle); at most `max_bytes + 1`
/// bytes are read; the SHA-256 of what was read is one of `accepted`.
fn read_verified_model(
    path: &Path,
    accepted: &[String],
    max_bytes: u64,
) -> Result<Vec<u8>, VisionError> {
    if accepted.is_empty() {
        return Err(refuse(path, "no SHA-256 digest is pinned for it"));
    }
    if !path.is_absolute() {
        return Err(refuse(path, "the path is not absolute"));
    }
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(refuse(path, "the path has no file name"));
    };

    let mut anchor = dir.to_path_buf();
    let mut tail = Vec::new();
    while tail.len() < LINK_CHECKED_DIR_LEVELS {
        let Some(part) = anchor.file_name().map(OsStr::to_os_string) else {
            break;
        };
        anchor.pop();
        tail.push(part);
    }
    let mut resolved = fs::canonicalize(&anchor)
        .map_err(|e| refuse(path, &format!("its directory cannot be resolved: {e}")))?;
    for part in tail.iter().rev() {
        resolved.push(part);
        let meta = fs::symlink_metadata(&resolved)
            .map_err(|e| refuse(path, &format!("its directory cannot be inspected: {e}")))?;
        if is_link_or_reparse(&meta) {
            return Err(refuse(
                path,
                "its directory path goes through a link or reparse point",
            ));
        }
        if !meta.is_dir() {
            return Err(refuse(path, "its directory path is not a directory"));
        }
    }
    resolved.push(name);

    let too_large = || refuse(path, &format!("it is larger than {max_bytes} bytes"));
    let meta = fs::symlink_metadata(&resolved)
        .map_err(|e| refuse(path, &format!("it cannot be inspected: {e}")))?;
    if is_link_or_reparse(&meta) {
        return Err(refuse(path, "it is a symbolic link or reparse point"));
    }
    if !meta.is_file() {
        return Err(refuse(path, "it is not a regular file"));
    }
    if meta.len() > max_bytes {
        return Err(too_large());
    }

    let file = fs::File::open(&resolved)
        .map_err(|e| refuse(path, &format!("it cannot be opened: {e}")))?;
    let opened = file
        .metadata()
        .map_err(|e| refuse(path, &format!("it cannot be inspected: {e}")))?;
    if !opened.is_file() {
        return Err(refuse(path, "it is not a regular file"));
    }
    if opened.len() > max_bytes {
        return Err(too_large());
    }
    let bytes = read_capped(file, max_bytes, opened.len())
        .map_err(|e| refuse(path, &format!("it cannot be read: {e}")))?
        .ok_or_else(too_large)?;

    let digest = sha256_hex(&bytes);
    if !accepted.iter().any(|d| d.eq_ignore_ascii_case(&digest)) {
        return Err(refuse(
            path,
            &format!("its SHA-256 ({digest}) is not the pinned digest"),
        ));
    }
    Ok(bytes)
}

/// Opt-in helper that downloads the `ocrs` detection and recognition models into `dir`
/// unless a file that passes [`OcrModelPaths::verify`] is already there.
///
/// Every download is checked against the pinned SHA-256 digest and size cap before it is moved
/// into place; a download that fails the check is deleted, never left behind. A file already in
/// place that fails the check is replaced by a fresh, verified download. `dir` must be an
/// absolute path.
///
/// Must be called explicitly by the application or CLI; unit tests never invoke this
/// automatically.
///
/// # Errors
///
/// Returns [`VisionError`] if `dir` is not absolute, or directory creation, downloading or
/// verification fails.
pub fn download_models_to(dir: impl AsRef<Path>) -> Result<OcrModelPaths, VisionError> {
    let dir = dir.as_ref();
    if !dir.is_absolute() {
        return Err(VisionError::Ocr(format!(
            "model directory {} is not an absolute path",
            dir.display()
        )));
    }
    fs::create_dir_all(dir)?;
    let paths = OcrModelPaths::from_dir(dir);
    let digests = ModelDigests::pinned();

    if read_verified_model(&paths.detection_model, &digests.detection, MAX_MODEL_BYTES).is_err() {
        download_file(
            DETECTION_MODEL_URL,
            &paths.detection_model,
            &digests.detection,
        )?;
    }
    if read_verified_model(
        &paths.recognition_model,
        &digests.recognition,
        MAX_MODEL_BYTES,
    )
    .is_err()
    {
        download_file(
            RECOGNITION_MODEL_URL,
            &paths.recognition_model,
            &digests.recognition,
        )?;
    }

    Ok(paths)
}

/// Opt-in helper that downloads the default `ocrs` models into [`OcrModelPaths::default_data_dir`]
/// on first use and returns their paths.
///
/// # Errors
///
/// Returns [`VisionError`] if there is no default data directory or downloading fails.
pub fn ensure_default_models() -> Result<OcrModelPaths, VisionError> {
    download_models_to(default_dir()?)
}

fn default_dir() -> Result<PathBuf, VisionError> {
    OcrModelPaths::default_data_dir().ok_or_else(|| {
        VisionError::Ocr("no absolute per-user data directory for the OCR models".to_string())
    })
}

fn download_file(url: &str, destination: &Path, accepted: &[String]) -> Result<(), VisionError> {
    let tmp_path = destination.with_extension("onnx.part");
    // A leftover part file could be a link that curl would write through.
    match fs::remove_file(&tmp_path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let status = Command::new("curl")
        .args(["-fSL", "--retry", "2", "--max-filesize"])
        .arg(MAX_MODEL_BYTES.to_string())
        .arg("-o")
        .arg(&tmp_path)
        .arg(url)
        .status()
        .map_err(|e| VisionError::Ocr(format!("failed to invoke curl to download {url}: {e}")))?;

    if !status.success() {
        let _ = fs::remove_file(&tmp_path);
        return Err(VisionError::Ocr(format!(
            "curl download failed for {url} with status {status}"
        )));
    }

    install_verified(&tmp_path, destination, accepted)
}

/// Moves the downloaded `tmp_path` to `destination` only if it passes the model checks;
/// otherwise deletes it. Loading re-verifies the bytes, so this is not the last line of defence.
fn install_verified(
    tmp_path: &Path,
    destination: &Path,
    accepted: &[String],
) -> Result<(), VisionError> {
    if let Err(e) = read_verified_model(tmp_path, accepted, MAX_MODEL_BYTES) {
        let _ = fs::remove_file(tmp_path);
        return Err(e);
    }
    fs::rename(tmp_path, destination)?;
    Ok(())
}

fn load_verified_model(
    path: &Path,
    accepted: &[String],
    label: &str,
) -> Result<Model, VisionError> {
    let bytes = read_verified_model(path, accepted, MAX_MODEL_BYTES)?;
    Model::load(bytes).map_err(|e| {
        VisionError::Ocr(format!(
            "failed to load {label} model from {}: {e}",
            path.display()
        ))
    })
}

/// Pure-Rust OCR engine backed by the `ocrs` and `rten` crates.
pub struct OcrsEngine {
    engine: InnerOcrsEngine,
    granularity: OcrGranularity,
}

impl OcrsEngine {
    /// Loads the `ocrs` engine from explicit detection and recognition model paths on disk,
    /// after vetting both files against the digests pinned in this build
    /// ([`OcrModelPaths::verify`]). Does not perform any network access.
    ///
    /// # Errors
    ///
    /// Returns [`VisionError::Ocr`] if a model file is missing, refused by the checks, or cannot
    /// be loaded.
    pub fn from_paths(paths: &OcrModelPaths) -> Result<Self, VisionError> {
        Self::from_paths_with_digests(paths, &ModelDigests::pinned())
    }

    /// Like [`OcrsEngine::from_paths`] with explicit accepted `digests`.
    ///
    /// Each model is read once, hashed, and loaded from those same in-memory bytes (not by
    /// opening the path again), so a file swapped after the check is never loaded. Because the
    /// bytes come from memory, an ONNX model cannot pull in other files as external tensor data.
    ///
    /// # Errors
    ///
    /// Returns [`VisionError::Ocr`] if a model file is missing, refused by the checks, or cannot
    /// be loaded.
    pub fn from_paths_with_digests(
        paths: &OcrModelPaths,
        digests: &ModelDigests,
    ) -> Result<Self, VisionError> {
        let detection_model =
            load_verified_model(&paths.detection_model, &digests.detection, "detection")?;
        let recognition_model = load_verified_model(
            &paths.recognition_model,
            &digests.recognition,
            "recognition",
        )?;

        let engine = InnerOcrsEngine::new(OcrEngineParams {
            detection_model: Some(detection_model),
            recognition_model: Some(recognition_model),
            ..Default::default()
        })
        .map_err(|e| VisionError::Ocr(format!("failed to initialize ocrs engine: {e}")))?;

        Ok(Self {
            engine,
            granularity: OcrGranularity::Line,
        })
    }

    /// Loads the `ocrs` engine from `dir` using the standard model filenames, without downloading.
    ///
    /// # Errors
    ///
    /// Returns [`VisionError::Ocr`] if the model files are missing or invalid.
    pub fn from_dir(dir: impl AsRef<Path>) -> Result<Self, VisionError> {
        Self::from_paths(&OcrModelPaths::from_dir(dir))
    }

    /// Loads the `ocrs` engine from [`OcrModelPaths::default_data_dir`] without downloading.
    ///
    /// # Errors
    ///
    /// Returns [`VisionError::Ocr`] if there is no default data directory or the models have not
    /// been downloaded yet.
    pub fn from_default_dir() -> Result<Self, VisionError> {
        Self::from_dir(default_dir()?)
    }

    /// Opt-in helper that downloads the models into `dir` if missing and then initializes [`OcrsEngine`].
    ///
    /// # Errors
    ///
    /// Returns [`VisionError`] if model download or initialization fails.
    pub fn load_or_download(dir: impl AsRef<Path>) -> Result<Self, VisionError> {
        let paths = download_models_to(dir)?;
        Self::from_paths(&paths)
    }

    /// Sets whether [`OcrsEngine::recognize`] outputs full lines or individual words.
    #[must_use]
    pub const fn with_granularity(mut self, granularity: OcrGranularity) -> Self {
        self.granularity = granularity;
        self
    }

    /// Fallible recognition method returning [`Result<Vec<TextBox>, VisionError>`].
    ///
    /// # Errors
    ///
    /// Returns [`VisionError::Ocr`] if image preparation, detection, or recognition fails.
    pub fn try_recognize(&self, img: &RgbImage) -> Result<Vec<TextBox>, VisionError> {
        let (img_w, img_h) = img.dimensions();
        if img_w == 0 || img_h == 0 {
            return Ok(Vec::new());
        }

        let source = ImageSource::from_bytes(img.as_raw(), img.dimensions())
            .map_err(|e| VisionError::Ocr(format!("invalid image source: {e}")))?;
        let ocr_input = self
            .engine
            .prepare_input(source)
            .map_err(|e| VisionError::Ocr(format!("failed to prepare OCR input: {e}")))?;

        let word_rects = self
            .engine
            .detect_words(&ocr_input)
            .map_err(|e| VisionError::Ocr(format!("word detection failed: {e}")))?;
        let line_rects = self.engine.find_text_lines(&ocr_input, &word_rects);
        let line_texts = self
            .engine
            .recognize_text(&ocr_input, &line_rects)
            .map_err(|e| VisionError::Ocr(format!("text recognition failed: {e}")))?;

        let mut boxes = Vec::new();
        for line in line_texts.into_iter().flatten() {
            match self.granularity {
                OcrGranularity::Line => {
                    let text = line.to_string();
                    if !text.trim().is_empty()
                        && let Some(bbox) = rten_rect_to_bbox(line.bounding_rect(), img_w, img_h)
                    {
                        boxes.push(TextBox::new(text, bbox, 1.0));
                    }
                }
                OcrGranularity::Word => {
                    for word in line.words() {
                        let text = word.to_string();
                        if !text.trim().is_empty()
                            && let Some(bbox) =
                                rten_rect_to_bbox(word.bounding_rect(), img_w, img_h)
                        {
                            boxes.push(TextBox::new(text, bbox, 1.0));
                        }
                    }
                }
            }
        }

        Ok(boxes)
    }
}

impl OcrEngine for OcrsEngine {
    fn recognize(&self, img: &RgbImage) -> Vec<TextBox> {
        self.try_recognize(img).unwrap_or_default()
    }
}

#[allow(clippy::cast_sign_loss)]
fn rten_rect_to_bbox(rect: rten_imageproc::Rect, img_w: u32, img_h: u32) -> Option<BoundingBox> {
    let left = rect.left().max(0) as u32;
    let top = rect.top().max(0) as u32;
    let right = (rect.right().max(0) as u32).min(img_w);
    let bottom = (rect.bottom().max(0) as u32).min(img_h);
    if right <= left || bottom <= top {
        return None;
    }
    Some(BoundingBox::from_ltrb(left, top, right, bottom))
}

/// Windows-native OCR engine backed by `Windows.Media.Ocr` (`windows` crate).
#[cfg(all(windows, feature = "windows-ocr"))]
pub struct WindowsOcrEngine {
    engine: windows::Media::Ocr::OcrEngine,
    granularity: OcrGranularity,
}

#[cfg(all(windows, feature = "windows-ocr"))]
impl WindowsOcrEngine {
    /// Creates a new [`WindowsOcrEngine`] using the current user's profile languages.
    ///
    /// # Errors
    ///
    /// Returns [`VisionError::Ocr`] if the Windows OCR engine cannot be initialized.
    pub fn new() -> Result<Self, VisionError> {
        let engine = windows::Media::Ocr::OcrEngine::TryCreateFromUserProfileLanguages()
            .map_err(|e| VisionError::Ocr(format!("Windows.Media.Ocr init failed: {e}")))?;
        Ok(Self {
            engine,
            granularity: OcrGranularity::Line,
        })
    }

    /// Sets whether [`WindowsOcrEngine::recognize`] outputs lines or individual words.
    #[must_use]
    pub const fn with_granularity(mut self, granularity: OcrGranularity) -> Self {
        self.granularity = granularity;
        self
    }

    /// Fallible recognition using `Windows.Media.Ocr`.
    ///
    /// # Errors
    ///
    /// Returns [`VisionError::Ocr`] if bitmap conversion or `WinRT` OCR fails.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn try_recognize(&self, img: &RgbImage) -> Result<Vec<TextBox>, VisionError> {
        use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
        use windows::Security::Cryptography::CryptographicBuffer;

        let (img_w, img_h) = img.dimensions();
        if img_w == 0 || img_h == 0 {
            return Ok(Vec::new());
        }

        let width_i32 = i32::try_from(img_w)
            .map_err(|_| VisionError::Ocr("image width exceeds i32::MAX".to_string()))?;
        let height_i32 = i32::try_from(img_h)
            .map_err(|_| VisionError::Ocr("image height exceeds i32::MAX".to_string()))?;

        let mut bgra = Vec::with_capacity((img_w as usize) * (img_h as usize) * 4);
        for pixel in img.pixels() {
            bgra.extend_from_slice(&[pixel.0[2], pixel.0[1], pixel.0[0], 255]);
        }

        let ibuffer = CryptographicBuffer::CreateFromByteArray(&bgra)
            .map_err(|e| VisionError::Ocr(format!("CreateFromByteArray failed: {e}")))?;
        let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
            &ibuffer,
            BitmapPixelFormat::Bgra8,
            width_i32,
            height_i32,
        )
        .map_err(|e| VisionError::Ocr(format!("CreateCopyFromBuffer failed: {e}")))?;

        let result = self
            .engine
            .RecognizeAsync(&bitmap)
            .map_err(|e| VisionError::Ocr(format!("RecognizeAsync start failed: {e}")))?
            .get()
            .map_err(|e| VisionError::Ocr(format!("RecognizeAsync completion failed: {e}")))?;

        let lines = result
            .Lines()
            .map_err(|e| VisionError::Ocr(format!("OcrResult::Lines failed: {e}")))?;

        let mut boxes = Vec::new();
        for line in lines {
            let words = line
                .Words()
                .map_err(|e| VisionError::Ocr(format!("OcrLine::Words failed: {e}")))?;

            match self.granularity {
                OcrGranularity::Word => {
                    for word in words {
                        let text = word
                            .Text()
                            .map_err(|e| VisionError::Ocr(format!("OcrWord::Text failed: {e}")))?
                            .to_string_lossy();
                        let rect = word.BoundingRect().map_err(|e| {
                            VisionError::Ocr(format!("OcrWord::BoundingRect failed: {e}"))
                        })?;
                        if !text.trim().is_empty() {
                            let x = (rect.X.max(0.0).round() as u32).min(img_w);
                            let y = (rect.Y.max(0.0).round() as u32).min(img_h);
                            let w = (rect.Width.max(0.0).round() as u32).min(img_w - x);
                            let h = (rect.Height.max(0.0).round() as u32).min(img_h - y);
                            if w > 0 && h > 0 {
                                boxes.push(TextBox::new(text, BoundingBox::new(x, y, w, h), 1.0));
                            }
                        }
                    }
                }
                OcrGranularity::Line => {
                    let text = line
                        .Text()
                        .map_err(|e| VisionError::Ocr(format!("OcrLine::Text failed: {e}")))?
                        .to_string_lossy();
                    let mut min_x = u32::MAX;
                    let mut min_y = u32::MAX;
                    let mut max_r = 0_u32;
                    let mut max_b = 0_u32;
                    for word in words {
                        if let Ok(rect) = word.BoundingRect() {
                            let x = (rect.X.max(0.0).round() as u32).min(img_w);
                            let y = (rect.Y.max(0.0).round() as u32).min(img_h);
                            let r = ((rect.X + rect.Width).max(0.0).round() as u32).min(img_w);
                            let b = ((rect.Y + rect.Height).max(0.0).round() as u32).min(img_h);
                            min_x = min_x.min(x);
                            min_y = min_y.min(y);
                            max_r = max_r.max(r);
                            max_b = max_b.max(b);
                        }
                    }
                    if !text.trim().is_empty() && max_r > min_x && max_b > min_y {
                        boxes.push(TextBox::new(
                            text,
                            BoundingBox::from_ltrb(min_x, min_y, max_r, max_b),
                            1.0,
                        ));
                    }
                }
            }
        }

        Ok(boxes)
    }
}

#[cfg(all(windows, feature = "windows-ocr"))]
impl OcrEngine for WindowsOcrEngine {
    fn recognize(&self, img: &RgbImage) -> Vec<TextBox> {
        self.try_recognize(img).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use image::Rgb;

    use super::*;
    use crate::canvas::draw_text;

    struct MockOcrEngine;

    impl OcrEngine for MockOcrEngine {
        fn recognize(&self, _img: &RgbImage) -> Vec<TextBox> {
            vec![TextBox::new(
                "Submit".to_string(),
                BoundingBox::new(10, 20, 60, 24),
                0.98,
            )]
        }
    }

    #[test]
    fn ocr_trait_and_model_path_resolution_work_without_network() {
        let engine = MockOcrEngine;
        let img = RgbImage::from_pixel(100, 100, Rgb([255, 255, 255]));
        let results = engine.recognize(&img);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].text, "Submit");
        assert_eq!(results[0].bbox, BoundingBox::new(10, 20, 60, 24));

        let paths = OcrModelPaths::from_dir("custom_model_dir");
        assert!(paths.detection_model.ends_with(DETECTION_MODEL_FILENAME));
        assert!(
            paths
                .recognition_model
                .ends_with(RECOGNITION_MODEL_FILENAME)
        );
    }

    fn digest_of(bytes: &[u8]) -> Vec<String> {
        vec![sha256_hex(bytes)]
    }

    /// A model directory laid out like the default one, below a fresh temp dir.
    fn model_dir(root: &Path) -> PathBuf {
        let dir = root.join("praxi-bot").join("models").join("ocrs");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn data_dir_ignores_the_user_cache_folder_even_when_models_are_there() {
        let home = tempfile::tempdir().unwrap();
        let cache = home.path().join(".cache").join("ocrs");
        fs::create_dir_all(&cache).unwrap();
        let planted = OcrModelPaths::from_dir(&cache);
        fs::write(&planted.detection_model, b"planted").unwrap();
        fs::write(&planted.recognition_model, b"planted").unwrap();

        let dir = resolve_data_dir(None, None, None, Some(home.path().as_os_str())).unwrap();

        assert!(!dir.starts_with(home.path().join(".cache")), "{dir:?}");
        assert_eq!(dir, model_dir(&home.path().join(".local").join("share")));
    }

    #[test]
    fn a_daemon_keeps_its_models_inside_its_own_data_dir_and_never_uses_a_relative_path() {
        let data = std::env::temp_dir().join("praxi-ocr-test-data");
        assert_eq!(
            resolve_model_dir(None, &data),
            Some(data.join("models").join("ocrs"))
        );
        // An absolute override wins; a relative or blank one is ignored.
        let elsewhere = std::env::temp_dir().join("praxi-ocr-test-elsewhere");
        assert_eq!(
            resolve_model_dir(Some(elsewhere.as_os_str()), &data),
            Some(elsewhere)
        );
        for bad in ["models/ocrs", "  ", ""] {
            assert_eq!(
                resolve_model_dir(Some(OsStr::new(bad)), &data),
                Some(data.join("models").join("ocrs")),
                "{bad:?}"
            );
        }
        // No absolute data dir, no model dir: there is no relative fallback.
        assert_eq!(resolve_model_dir(None, Path::new("relative/data")), None);
        assert_eq!(resolve_model_dir(None, Path::new("")), None);
    }

    #[test]
    fn data_dir_is_never_relative() {
        assert_eq!(resolve_data_dir(None, None, None, None), None);
        let relative = |s: &str| Some(OsString::from(s));
        let (local, xdg, home) = (
            relative("rel-local"),
            relative("rel-xdg"),
            relative("rel-home"),
        );
        assert_eq!(
            resolve_data_dir(
                relative("rel-override").as_deref(),
                local.as_deref(),
                xdg.as_deref(),
                home.as_deref(),
            ),
            None,
            "no relative fallback such as models/ocrs"
        );
    }

    #[test]
    fn a_relative_model_dir_override_is_ignored_and_an_absolute_one_is_used() {
        let base = tempfile::tempdir().unwrap();
        let base_os = base.path().as_os_str();
        assert_eq!(
            resolve_data_dir(Some(OsStr::new("models/ocrs")), Some(base_os), None, None),
            Some(model_dir(base.path())),
            "a relative override falls through to the per-user data dir"
        );
        let custom = base.path().join("my-models");
        assert_eq!(
            resolve_data_dir(Some(custom.as_os_str()), Some(base_os), None, None),
            Some(custom)
        );
        assert_eq!(
            resolve_data_dir(Some(OsStr::new("  ")), Some(base_os), None, None),
            Some(model_dir(base.path())),
            "a blank override is no override"
        );
    }

    #[test]
    fn data_dir_prefers_local_app_data_then_xdg_then_home() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let c = tempfile::tempdir().unwrap();
        let (oa, ob, oc) = (
            a.path().as_os_str(),
            b.path().as_os_str(),
            c.path().as_os_str(),
        );
        assert_eq!(
            resolve_data_dir(None, Some(oa), Some(ob), Some(oc)),
            Some(model_dir(a.path()))
        );
        assert_eq!(
            resolve_data_dir(None, Some(OsStr::new("rel")), Some(ob), Some(oc)),
            Some(model_dir(b.path()))
        );
        assert_eq!(
            resolve_data_dir(None, None, None, Some(oc)),
            Some(model_dir(&c.path().join(".local").join("share")))
        );
    }

    #[test]
    fn the_pinned_digests_are_sha256_hex_and_both_present() {
        for digests in [DETECTION_MODEL_SHA256, RECOGNITION_MODEL_SHA256] {
            assert!(!digests.is_empty(), "TODO(pin): digests missing");
            for d in digests {
                assert_eq!(d.len(), 64, "{d}");
                assert!(
                    d.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                    "{d}"
                );
            }
        }
    }

    #[test]
    fn a_model_with_the_pinned_digest_is_accepted_and_its_bytes_returned() {
        let root = tempfile::tempdir().unwrap();
        let path = model_dir(root.path()).join(DETECTION_MODEL_FILENAME);
        fs::write(&path, b"model bytes").unwrap();
        let bytes = read_verified_model(&path, &digest_of(b"model bytes"), 1024).unwrap();
        assert_eq!(bytes, b"model bytes");
        // Hex case does not matter.
        let upper = vec![sha256_hex(b"model bytes").to_ascii_uppercase()];
        assert!(read_verified_model(&path, &upper, 1024).is_ok());
    }

    #[test]
    fn a_model_with_the_wrong_digest_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let path = model_dir(root.path()).join(DETECTION_MODEL_FILENAME);
        fs::write(&path, b"forged model").unwrap();
        let err = read_verified_model(&path, &digest_of(b"real model"), 1024).unwrap_err();
        assert!(err.to_string().contains("not the pinned digest"), "{err}");
    }

    #[test]
    fn with_no_pinned_digest_nothing_is_accepted() {
        let root = tempfile::tempdir().unwrap();
        let path = model_dir(root.path()).join(DETECTION_MODEL_FILENAME);
        fs::write(&path, b"anything").unwrap();
        let err = read_verified_model(&path, &[], 1024).unwrap_err();
        assert!(err.to_string().contains("no SHA-256 digest"), "{err}");
    }

    #[test]
    fn a_model_over_the_size_cap_is_refused_before_it_is_read() {
        let root = tempfile::tempdir().unwrap();
        let path = model_dir(root.path()).join(DETECTION_MODEL_FILENAME);
        let big = vec![7_u8; 64];
        fs::write(&path, &big).unwrap();
        // The digest is right, so only the size can be the reason.
        let err = read_verified_model(&path, &digest_of(&big), 16).unwrap_err();
        assert!(err.to_string().contains("larger than 16 bytes"), "{err}");
    }

    #[test]
    fn a_capped_read_never_reads_past_the_cap_even_from_an_endless_source() {
        assert_eq!(read_capped(io::repeat(7), 1024, 0).unwrap(), None);
        assert_eq!(
            read_capped(&b"abc"[..], 3, 3).unwrap().as_deref(),
            Some(&b"abc"[..])
        );
        assert_eq!(read_capped(&b"abcd"[..], 3, 0).unwrap(), None);
    }

    #[test]
    fn a_relative_model_path_is_refused() {
        let err = read_verified_model(
            Path::new("models/ocrs/text-detection.onnx"),
            &digest_of(b"x"),
            1024,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not absolute"), "{err}");
    }

    #[test]
    fn a_missing_model_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let path = model_dir(root.path()).join(DETECTION_MODEL_FILENAME);
        assert!(read_verified_model(&path, &digest_of(b"x"), 1024).is_err());
    }

    #[cfg(any(unix, windows))]
    fn symlink_file(target: &Path, link: &Path) -> io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(target, link)
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn a_symlink_to_a_valid_looking_model_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let dir = model_dir(root.path());
        let real = root.path().join("real.onnx");
        fs::write(&real, b"model bytes").unwrap();
        let link = dir.join(DETECTION_MODEL_FILENAME);
        if let Err(e) = symlink_file(&real, &link) {
            // Creating symlinks on Windows needs a privilege (or developer mode): skip there.
            // The directory-junction test below covers Windows reparse points without one.
            #[cfg(windows)]
            {
                eprintln!("skipped: no symlink privilege ({e})");
                return;
            }
            #[cfg(not(windows))]
            panic!("symlink creation failed: {e}");
        }
        let err = read_verified_model(&link, &digest_of(b"model bytes"), 1024).unwrap_err();
        assert!(err.to_string().contains("symbolic link"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_model_behind_a_symlinked_directory_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let elsewhere = root.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join(DETECTION_MODEL_FILENAME), b"model bytes").unwrap();
        let models = root.path().join("praxi-bot").join("models");
        fs::create_dir_all(&models).unwrap();
        std::os::unix::fs::symlink(&elsewhere, models.join("ocrs")).unwrap();
        let path = models.join("ocrs").join(DETECTION_MODEL_FILENAME);
        let err = read_verified_model(&path, &digest_of(b"model bytes"), 1024).unwrap_err();
        assert!(err.to_string().contains("link or reparse point"), "{err}");
    }

    // A directory junction is a reparse point that needs no privilege to create.
    #[cfg(windows)]
    #[test]
    fn a_model_behind_a_directory_junction_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let elsewhere = root.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join(DETECTION_MODEL_FILENAME), b"model bytes").unwrap();
        let models = root.path().join("praxi-bot").join("models");
        fs::create_dir_all(&models).unwrap();
        let junction = models.join("ocrs");
        let made = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&elsewhere)
            .output()
            .unwrap();
        assert!(made.status.success(), "mklink /J failed");
        let path = junction.join(DETECTION_MODEL_FILENAME);
        let err = read_verified_model(&path, &digest_of(b"model bytes"), 1024).unwrap_err();
        assert!(err.to_string().contains("link or reparse point"), "{err}");
    }

    #[test]
    fn the_engine_refuses_files_that_are_not_the_pinned_models_before_loading_them() {
        let root = tempfile::tempdir().unwrap();
        let paths = OcrModelPaths::from_dir(model_dir(root.path()));
        fs::write(&paths.detection_model, b"not a model").unwrap();
        fs::write(&paths.recognition_model, b"not a model").unwrap();
        let Err(err) = OcrsEngine::from_paths(&paths) else {
            panic!("forged models must not load");
        };
        let msg = err.to_string();
        assert!(msg.contains("not the pinned digest"), "{msg}");
        assert!(
            !msg.contains("failed to load"),
            "refused before any load: {msg}"
        );
        assert!(paths.verify().is_err());
    }

    #[test]
    fn the_engine_loads_from_the_verified_bytes_and_reports_a_bad_model_as_such() {
        let root = tempfile::tempdir().unwrap();
        let paths = OcrModelPaths::from_dir(model_dir(root.path()));
        fs::write(&paths.detection_model, b"digest-matching junk").unwrap();
        fs::write(&paths.recognition_model, b"digest-matching junk").unwrap();
        let digests = ModelDigests {
            detection: digest_of(b"digest-matching junk"),
            recognition: digest_of(b"digest-matching junk"),
        };
        assert!(paths.verify_with(&digests).is_ok());
        let Err(err) = OcrsEngine::from_paths_with_digests(&paths, &digests) else {
            panic!("junk is not a model");
        };
        assert!(
            err.to_string().contains("failed to load detection model"),
            "{err}"
        );
    }

    #[test]
    fn a_download_that_fails_verification_is_deleted_and_never_installed() {
        let root = tempfile::tempdir().unwrap();
        let dir = model_dir(root.path());
        let tmp = dir.join("text-detection.onnx.part");
        let destination = dir.join(DETECTION_MODEL_FILENAME);
        fs::write(&tmp, b"tampered download").unwrap();
        assert!(install_verified(&tmp, &destination, &digest_of(b"real model")).is_err());
        assert!(!tmp.exists(), "the unverified download is removed");
        assert!(!destination.exists(), "and nothing is installed");

        fs::write(&tmp, b"real model").unwrap();
        install_verified(&tmp, &destination, &digest_of(b"real model")).unwrap();
        assert!(!tmp.exists());
        assert_eq!(fs::read(&destination).unwrap(), b"real model");
    }

    #[test]
    fn downloading_into_a_relative_directory_is_refused_before_anything_happens() {
        let name = "relative-ocr-dir-that-must-not-be-created";
        assert!(download_models_to(name).is_err());
        assert!(!Path::new(name).exists());
    }

    #[test]
    #[ignore = "Requires local ocrs ONNX model files in default_data_dir()"]
    fn ocrs_engine_recognizes_rendered_text_when_models_present() {
        let engine = OcrsEngine::from_default_dir()
            .expect("ocrs models must be present in default_data_dir() to run this ignored test");
        let mut img = RgbImage::from_pixel(400, 120, Rgb([255, 255, 255]));
        draw_text(&mut img, 30, 40, "HELLO RUST", Rgb([0, 0, 0]), 4);

        let boxes = engine.recognize(&img);
        assert!(
            !boxes.is_empty(),
            "expected OcrsEngine to recognize rendered text"
        );
    }

    #[cfg(all(windows, feature = "windows-ocr"))]
    #[test]
    fn windows_ocr_engine_recognizes_sample_image() {
        let engine = WindowsOcrEngine::new().expect("Windows.Media.Ocr should initialize");
        let mut img = RgbImage::from_pixel(400, 120, Rgb([255, 255, 255]));
        draw_text(&mut img, 30, 40, "WINDOWS OCR", Rgb([0, 0, 0]), 4);
        let _boxes = engine.recognize(&img);
    }
}
