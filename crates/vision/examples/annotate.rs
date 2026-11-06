//! Example CLI that loads an image, runs OCR, renders Set-of-Marks and Grid overlays
//! to `out_marks.jpg` and `out_grid.jpg`, and prints the detected text boxes.
//!
//! Usage:
//!   cargo run -p vision --example annotate -- <image> [--download-models]

use std::env;
use std::error::Error;
use std::fs;
use std::process;

use vision::{
    GridConfig, OcrEngine, OcrModelPaths, OcrsEngine, annotate_text_boxes, draw_grid, encode_jpeg,
};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let mut image_path: Option<String> = None;
    let mut download_models = false;

    for arg in &mut args {
        match arg.as_str() {
            "--download-models" => download_models = true,
            "-h" | "--help" => {
                println!("Usage: annotate <image> [--download-models]");
                process::exit(0);
            }
            _ if image_path.is_none() => image_path = Some(arg),
            other => {
                eprintln!("Unknown argument: {other}");
                eprintln!("Usage: annotate <image> [--download-models]");
                process::exit(2);
            }
        }
    }

    let Some(image_path) = image_path else {
        eprintln!("Usage: annotate <image> [--download-models]");
        process::exit(2);
    };

    let img = image::open(&image_path)?.into_rgb8();
    let (width, height) = img.dimensions();
    println!("Loaded {image_path} ({width}x{height})");

    let model_dir = OcrModelPaths::default_data_dir();
    let engine_result = if download_models {
        println!(
            "Ensuring ocrs models are downloaded in {}...",
            model_dir.display()
        );
        OcrsEngine::load_or_download(&model_dir)
    } else {
        OcrsEngine::from_dir(&model_dir)
    };

    let boxes = match engine_result {
        Ok(engine) => engine.recognize(&img),
        Err(err) => {
            #[cfg(all(windows, feature = "windows-ocr"))]
            {
                eprintln!("ocrs models unavailable ({err}); falling back to Windows.Media.Ocr...");
                let win_engine = vision::WindowsOcrEngine::new()?;
                win_engine.recognize(&img)
            }
            #[cfg(not(all(windows, feature = "windows-ocr")))]
            {
                eprintln!(
                    "Note: ocrs models not found in {} ({err}). Pass --download-models to download them.",
                    model_dir.display()
                );
                Vec::new()
            }
        }
    };

    println!("Detected {} text box(es):", boxes.len());
    for (idx, tb) in boxes.iter().enumerate() {
        let label = idx + 1;
        println!(
            "  [{label}] {:?} bbox=(x={}, y={}, w={}, h={}) conf={:.2}",
            tb.text, tb.bbox.x, tb.bbox.y, tb.bbox.width, tb.bbox.height, tb.confidence
        );
    }

    let marks_result = annotate_text_boxes(&img, &boxes);
    let marks_jpg = encode_jpeg(&marks_result.image, 90)?;
    fs::write("out_marks.jpg", &marks_jpg.bytes)?;
    println!("Wrote out_marks.jpg ({} bytes)", marks_jpg.bytes.len());

    let grid_result = draw_grid(&img, &GridConfig::default());
    let grid_jpg = encode_jpeg(&grid_result.image, 90)?;
    fs::write("out_grid.jpg", &grid_jpg.bytes)?;
    println!("Wrote out_grid.jpg ({} bytes)", grid_jpg.bytes.len());

    Ok(())
}
