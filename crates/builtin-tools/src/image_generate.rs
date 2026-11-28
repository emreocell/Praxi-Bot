//! Provider-backed image generation into the bot workspace.

use image::ImageEncoder;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{
    Effect, PermissionCategory, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::util::{exec_err, finish_text, parse, spec};
use crate::{FsAccessProvider, FsScope};

pub const IMAGE_GENERATE: &str = "image_generate";

/// Generate an image and return raw bytes plus optional usage metadata.
#[derive(Debug, Clone)]
pub struct GeneratedImage {
    pub bytes: Vec<u8>,
    pub mime_type: String,
    pub model: String,
    pub revised_prompt: Option<String>,
}

/// Runs the provider call under the agent loop (spend limits, ledger, secrets).
#[async_trait]
pub trait ImageGenerateBackend: Send + Sync {
    /// Refuse when the prompt looks like a stored secret.
    ///
    /// # Errors
    /// When the prompt matches a known secret shape.
    fn prompt_allowed(&self, prompt: &str) -> Result<(), String>;

    /// One provider attempt with ledger hooks already applied by the host.
    ///
    /// # Errors
    /// Provider, limit, or configuration errors (plain text).
    async fn generate(
        &self,
        run_id: &str,
        bot_id: Option<&str>,
        model: &str,
        prompt: &str,
        size: Option<&str>,
    ) -> Result<GeneratedImage, String>;
}

/// Owner-visible preview path for this run (cleared after the loop reads it).
#[derive(Debug, Default)]
pub struct PreviewSideband {
    pub preview: Option<PathBuf>,
}

fn sideband_map() -> std::sync::MutexGuard<'static, HashMap<String, PreviewSideband>> {
    static MAP: OnceLock<Mutex<HashMap<String, PreviewSideband>>> = OnceLock::new();
    MAP.get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Record the saved image path for the owner UI (`shot` on the tool step).
pub fn note_preview(run_id: &str, path: &Path) {
    const MAX_RUNS: usize = 64;
    let mut m = sideband_map();
    if m.len() >= MAX_RUNS && !m.contains_key(run_id) {
        m.clear();
    }
    m.entry(run_id.to_owned()).or_default().preview = Some(path.to_path_buf());
}

/// Take and clear the preview recorded for `run_id`.
#[must_use]
pub fn take_preview(run_id: &str) -> PreviewSideband {
    sideband_map().remove(run_id).unwrap_or_default()
}

#[derive(Deserialize)]
struct Input {
    prompt: String,
    model: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    size: Option<String>,
}

pub struct ImageGenerateTool {
    spec: ToolSpec,
    backend: Arc<dyn ImageGenerateBackend>,
    fs_access: Option<Arc<dyn FsAccessProvider>>,
    fs_home: Option<PathBuf>,
}

#[must_use]
pub fn tool(
    backend: Arc<dyn ImageGenerateBackend>,
    fs_access: Option<Arc<dyn FsAccessProvider>>,
    fs_home: Option<PathBuf>,
) -> Arc<dyn Tool> {
    Arc::new(ImageGenerateTool {
        spec: spec(
            IMAGE_GENERATE,
            "Generate an image from a text prompt using a configured image model (OpenAI-compatible or Gemini Imagen), and save the result as a PNG in this bot's workspace. The prompt is untrusted: do not put secrets in it. Example: {\"prompt\":\"A simple logo for a coffee shop\",\"model\":\"openrouter/openai/gpt-image-1\",\"path\":\"downloads/logo.png\"}",
            json!({
                "type": "object",
                "required": ["prompt", "model"],
                "properties": {
                    "prompt": {"type": "string", "description": "What to draw (plain language, no secrets)"},
                    "model": {"type": "string", "description": "Full model reference provider/model (image-capable)"},
                    "path": {"type": "string", "description": "Workspace-relative path (default generated/<uuid>.png)"},
                    "size": {"type": "string", "description": "Optional size, e.g. 1024x1024 (OpenAI-compatible providers)"}
                }
            }),
            Effect::Write,
            PermissionCategory::Fs,
        ),
        backend,
        fs_access,
        fs_home,
    })
}

fn workspace_root(
    fs: &dyn FsAccessProvider,
    bot_id: Option<&str>,
    fs_home: Option<&Path>,
) -> Result<PathBuf, ToolError> {
    if let Some(ws) = fs.bot_workspace(bot_id) {
        return Ok(ws);
    }
    match fs.scope(bot_id) {
        FsScope::Folders(f) if !f.is_empty() => Ok(f[0].clone()),
        FsScope::Computer => fs_home
            .map(Path::to_path_buf)
            .ok_or_else(|| exec_err("no workspace or home directory for this bot")),
        FsScope::Folders(_) => Err(exec_err(
            "this bot has folder-only file access and no workspace folder",
        )),
    }
}

fn resolve_under_workspace(base: &Path, rel: &str) -> Result<PathBuf, ToolError> {
    let rel = rel.trim();
    if rel.is_empty() || rel.contains('\0') {
        return Err(ToolError::InvalidInput("invalid path".into()));
    }
    let path = Path::new(rel);
    if path.is_absolute() {
        return Err(ToolError::InvalidInput(
            "path must be relative to the workspace".into(),
        ));
    }
    for c in path.components() {
        match c {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(ToolError::InvalidInput(
                    "path must stay inside the workspace".into(),
                ));
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    let mut out = base.to_path_buf();
    out.push(path);
    Ok(out)
}

#[async_trait]
impl Tool for ImageGenerateTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn egress_extra(&self, _ctx: &ToolContext, input: &Value) -> Option<String> {
        let model = input.get("model").and_then(Value::as_str)?;
        Some(format!("image model {model}"))
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let i: Input = parse(input)?;
        if i.prompt.len() > provider_catalog::MAX_IMAGE_PROMPT_CHARS {
            return Err(ToolError::InvalidInput(
                "prompt exceeds the maximum length".into(),
            ));
        }
        self.backend
            .prompt_allowed(&i.prompt)
            .map_err(ToolError::Execution)?;
        let fs = self.fs_access.as_deref().ok_or_else(|| {
            ToolError::Execution("image generation is not available without file access".into())
        })?;
        let base = workspace_root(fs, ctx.bot_id.as_deref(), self.fs_home.as_deref())?;
        std::fs::create_dir_all(&base).map_err(|e| ToolError::Execution(e.to_string()))?;
        if let Err(reason) = fs.check_write(ctx.bot_id.as_deref(), &base, 4 * 1024 * 1024) {
            return Err(ToolError::Execution(reason));
        }
        let rel = i
            .path
            .unwrap_or_else(|| format!("generated/{}.png", uuid::Uuid::now_v7()));
        let target = resolve_under_workspace(&base, &rel)?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ToolError::Execution(e.to_string()))?;
        }
        if let Err(reason) = fs.check_write(ctx.bot_id.as_deref(), &target, 4 * 1024 * 1024) {
            return Err(ToolError::Execution(reason));
        }
        let generated = self
            .backend
            .generate(
                &ctx.run_id,
                ctx.bot_id.as_deref(),
                &i.model,
                &i.prompt,
                i.size.as_deref(),
            )
            .await
            .map_err(ToolError::Execution)?;
        let png = encode_png(&generated.bytes).map_err(ToolError::Execution)?;
        if let Err(reason) = fs.check_write(
            ctx.bot_id.as_deref(),
            &target,
            u64::try_from(png.len()).unwrap_or(u64::MAX),
        ) {
            return Err(ToolError::Execution(reason));
        }
        std::fs::write(&target, &png).map_err(|e| ToolError::Execution(e.to_string()))?;
        note_preview(&ctx.run_id, &target);
        let mut text = format!(
            "saved image to workspace path {rel} ({bytes} bytes, model {model})",
            bytes = png.len(),
            model = generated.model,
        );
        if let Some(revised) = generated.revised_prompt.filter(|s| !s.is_empty()) {
            let _ = writeln!(text, "revised_prompt: {revised}");
        }
        Ok(finish_text(ctx, &base.join(".spill"), text))
    }
}

fn encode_png(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        return Ok(bytes.to_vec());
    }
    let img = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| format!("could not decode provider image bytes: {e}"))?;
    let mut out = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut out);
    encoder
        .write_image(
            img.as_bytes(),
            img.width(),
            img.height(),
            img.color().into(),
        )
        .map_err(|e| e.to_string())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct FakeBackend {
        png: Vec<u8>,
    }

    #[async_trait]
    impl ImageGenerateBackend for FakeBackend {
        fn prompt_allowed(&self, prompt: &str) -> Result<(), String> {
            if prompt.contains("SECRET") {
                Err("prompt must not contain secrets".into())
            } else {
                Ok(())
            }
        }

        async fn generate(
            &self,
            _: &str,
            _: Option<&str>,
            model: &str,
            _: &str,
            _: Option<&str>,
        ) -> Result<GeneratedImage, String> {
            Ok(GeneratedImage {
                bytes: self.png.clone(),
                mime_type: "image/png".into(),
                model: model.to_owned(),
                revised_prompt: None,
            })
        }
    }

    struct LocalFs;

    impl FsAccessProvider for LocalFs {
        fn scope(&self, _: Option<&str>) -> FsScope {
            FsScope::Computer
        }

        fn bot_workspace(&self, _: Option<&str>) -> Option<PathBuf> {
            Some(std::env::temp_dir().join("praxi-img-test-ws"))
        }
    }

    #[tokio::test]
    async fn saves_png_and_notes_preview() {
        let png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0];
        let tool = tool(Arc::new(FakeBackend { png }), Some(Arc::new(LocalFs)), None);
        let run = uuid::Uuid::now_v7().to_string();
        let out = tool
            .call(
                &ToolContext {
                    run_id: run.clone(),
                    bot_id: Some("00000000-0000-0000-0000-000000000001".into()),
                    ..ToolContext::default()
                },
                json!({"prompt": "circle", "model": "fake/m", "path": "generated/t.png"}),
            )
            .await
            .unwrap();
        assert_eq!(out.content.len(), 1);
        let prev = take_preview(&run).preview.expect("preview path");
        assert!(prev.exists());
        let _ = std::fs::remove_file(prev);
    }

    #[tokio::test]
    async fn refuses_secrets_in_prompt() {
        let tool = tool(
            Arc::new(FakeBackend { png: vec![1, 2, 3] }),
            Some(Arc::new(LocalFs)),
            None,
        );
        let err = tool
            .call(
                &ToolContext::default(),
                json!({"prompt": "SECRET key", "model": "fake/m"}),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)));
    }
}
