//! Optical Character Recognition (OCR) trait and engine implementations (`ocrs` pure-Rust
//! engine and optional `Windows.Media.Ocr` native engine behind the `windows-ocr` feature).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use image::RgbImage;
use ocrs::{ImageSource, OcrEngine as InnerOcrsEngine, OcrEngineParams, TextItem};
use rten::Model;

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

    /// Returns the default local data directory for `ocrs` models.
    ///
    /// Resolution order:
    /// 1. `PRAXI_OCR_MODEL_DIR` environment variable if set.
    /// 2. `~/.cache/ocrs` if both default model files already exist there.
    /// 3. Platform data dir (`%LOCALAPPDATA%\praxi-bot\models\ocrs` on Windows,
    ///    `$XDG_DATA_HOME/praxi-bot/models/ocrs` or `~/.local/share/praxi-bot/models/ocrs` on Unix).
    #[must_use]
    pub fn default_data_dir() -> PathBuf {
        if let Ok(custom) = env::var("PRAXI_OCR_MODEL_DIR")
            && !custom.trim().is_empty()
        {
            return PathBuf::from(custom);
        }

        if let Some(home) = home_dir() {
            let ocrs_cache = home.join(".cache").join("ocrs");
            if Self::from_dir(&ocrs_cache).exists() {
                return ocrs_cache;
            }
        }

        if let Ok(local_app_data) = env::var("LOCALAPPDATA")
            && !local_app_data.trim().is_empty()
        {
            return PathBuf::from(local_app_data)
                .join("praxi-bot")
                .join("models")
                .join("ocrs");
        }

        if let Ok(xdg_data) = env::var("XDG_DATA_HOME")
            && !xdg_data.trim().is_empty()
        {
            return PathBuf::from(xdg_data)
                .join("praxi-bot")
                .join("models")
                .join("ocrs");
        }

        if let Some(home) = home_dir() {
            return home
                .join(".local")
                .join("share")
                .join("praxi-bot")
                .join("models")
                .join("ocrs");
        }

        PathBuf::from("models").join("ocrs")
    }

    /// Returns `true` if both model files exist on disk.
    #[must_use]
    pub fn exists(&self) -> bool {
        self.detection_model.is_file() && self.recognition_model.is_file()
    }
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Opt-in helper that downloads the `ocrs` detection and recognition models into `dir`
/// if they are not already present on disk.
///
/// Must be called explicitly by the application or CLI; unit tests never invoke this
/// automatically.
///
/// # Errors
///
/// Returns [`VisionError`] if directory creation or downloading fails.
pub fn download_models_to(dir: impl AsRef<Path>) -> Result<OcrModelPaths, VisionError> {
    let dir = dir.as_ref();
    fs::create_dir_all(dir)?;
    let paths = OcrModelPaths::from_dir(dir);

    if !paths.detection_model.is_file() {
        download_file(DETECTION_MODEL_URL, &paths.detection_model)?;
    }
    if !paths.recognition_model.is_file() {
        download_file(RECOGNITION_MODEL_URL, &paths.recognition_model)?;
    }

    Ok(paths)
}

/// Opt-in helper that downloads the default `ocrs` models into [`OcrModelPaths::default_data_dir`]
/// on first use and returns their paths.
///
/// # Errors
///
/// Returns [`VisionError`] if downloading fails.
pub fn ensure_default_models() -> Result<OcrModelPaths, VisionError> {
    download_models_to(OcrModelPaths::default_data_dir())
}

fn download_file(url: &str, destination: &Path) -> Result<(), VisionError> {
    let tmp_path = destination.with_extension("onnx.part");
    let status = Command::new("curl")
        .args(["-fSL", "--retry", "2", "-o"])
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

    fs::rename(&tmp_path, destination)?;
    Ok(())
}

/// Pure-Rust OCR engine backed by the `ocrs` and `rten` crates.
pub struct OcrsEngine {
    engine: InnerOcrsEngine,
    granularity: OcrGranularity,
}

impl OcrsEngine {
    /// Loads the `ocrs` engine from explicit detection and recognition model paths on disk.
    /// Does not perform any network access.
    ///
    /// # Errors
    ///
    /// Returns [`VisionError::Ocr`] if the model files do not exist or cannot be loaded.
    pub fn from_paths(paths: &OcrModelPaths) -> Result<Self, VisionError> {
        let detection_model = Model::load_file(&paths.detection_model).map_err(|e| {
            VisionError::Ocr(format!(
                "failed to load detection model from {}: {e}",
                paths.detection_model.display()
            ))
        })?;
        let recognition_model = Model::load_file(&paths.recognition_model).map_err(|e| {
            VisionError::Ocr(format!(
                "failed to load recognition model from {}: {e}",
                paths.recognition_model.display()
            ))
        })?;

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
    /// Returns [`VisionError::Ocr`] if the models have not been downloaded yet.
    pub fn from_default_dir() -> Result<Self, VisionError> {
        Self::from_dir(OcrModelPaths::default_data_dir())
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
