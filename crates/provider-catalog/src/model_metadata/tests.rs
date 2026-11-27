use super::*;

fn ids(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).into()).collect()
}

#[test]
fn curated_table_is_valid_and_covers_every_shipped_provider() {
    let table = embedded_metadata();
    assert_eq!(table.validate(), Ok(()));
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../providers/catalog");
    for provider in crate::Catalog::load_dir(&dir).unwrap().all() {
        assert!(
            table
                .providers
                .iter()
                .any(|p| p.id == provider.id || p.aliases.contains(&provider.id))
        );
    }
    for id in ["openai", "xai", "cli:claude", "cli:codex"] {
        assert!(table.providers.iter().any(|p| p.id == id));
    }
}

#[test]
fn curated_validation_rejects_duplicate_ids_ranks_and_nonchat_recommendations() {
    let source = include_str!("../model_metadata.json");
    let load = || serde_json::from_str::<CuratedModels>(source).unwrap();
    let mut table = load();
    let mut duplicate = table.providers[0].models[0].clone();
    // Isolate the id guard from recommendation-rank duplication so removing
    // id validation cannot be hidden by an unrelated validation failure.
    duplicate.recommended = false;
    duplicate.recommended_rank = None;
    table.providers[0].models.push(duplicate);
    assert!(table.validate().is_err());
    let mut table = load();
    table.providers[0].models[0].kind = ModelKind::Image;
    assert!(table.validate().is_err());
    let mut table = load();
    table.providers[0].models[0].recommended_rank = Some(5);
    assert!(table.validate().is_err());
    let mut table = load();
    table.providers[0].models[1].recommended_rank = Some(1);
    assert!(table.validate().is_err());
    let mut table = load();
    table.providers[0].models[0].deprecated = true;
    assert!(table.validate().is_err());
    let mut table = load();
    table.providers[0].models[0].id = "vendor/model:free".into();
    assert!(table.validate().is_err());
}

#[test]
fn recommendations_are_sorted_and_intersect_live_ids_without_inventing_models() {
    let list = model_metadata(
        "anthropic",
        &ids(&[
            "unknown-chat",
            "claude-haiku-5-5",
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-sonnet-5-5",
        ]),
        true,
    );
    assert_eq!(
        list.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        [
            "claude-sonnet-5-5",
            "claude-opus-5-5",
            "claude-haiku-5-5",
            "unknown-chat"
        ]
    );
    assert_eq!(list[0].display_name, "Claude Sonnet 5.5");
    assert!(!list[3].recommended);
    assert_eq!(model_metadata("anthropic", &[], true), Vec::new());
    assert!(!list.iter().any(|m| m.id == "claude-fable-5-2"));
}

#[test]
fn default_filter_hides_nonchat_and_deprecated_but_all_models_exposes_them() {
    let models = ids(&[
        "gemini-4-carbon",
        "gemini-2.0-flash",
        "gemini-2.5-flash-preview-tts",
        "text-embedding-004",
    ]);
    let visible = model_metadata("gemini", &models, true);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].id, "gemini-4-carbon");
    let all = model_metadata("gemini", &models, false);
    assert_eq!(all.len(), 4);
    assert!(
        all.iter()
            .any(|m| m.id == "gemini-2.0-flash" && m.deprecated)
    );
    assert_eq!(
        model_metadata("gemini-openai-compat", &models, true),
        visible
    );
}

#[test]
fn inference_separates_generation_and_special_endpoints_from_chat() {
    for (id, kind) in [
        ("text-embedding-3-large", ModelKind::Embedding),
        ("nomic-embed-text", ModelKind::Embedding),
        ("gpt-realtime", ModelKind::Audio),
        ("gpt-4o-audio-preview", ModelKind::Audio),
        ("gemini-3.8-flash-lite-tts", ModelKind::Audio),
        ("lyria-realtime-exp", ModelKind::Audio),
        ("gpt-whisper-transcribe", ModelKind::Audio),
        ("suno-music", ModelKind::Audio),
        ("gemini-3.8-live", ModelKind::Audio),
        ("gpt-image-1", ModelKind::Image),
        ("imagen-4.0-generate-001", ModelKind::Image),
        ("black-forest-labs/flux-2", ModelKind::Image),
        ("gemini-nano-banana-2.1", ModelKind::Image),
        ("gemini-omni-1.2-flash", ModelKind::Video),
        ("veo-3.1-generate-preview", ModelKind::Video),
        ("grok-imagine-video", ModelKind::Video),
        ("sora-2", ModelKind::Video),
        ("o3-deep-research", ModelKind::Other),
        ("omni-moderation-latest", ModelKind::Other),
        ("computer-use-preview", ModelKind::Other),
        ("rerank-v3", ModelKind::Other),
        ("muse-image-1.0", ModelKind::Image),
        ("muse-voice-transcribe-1.0", ModelKind::Audio),
        ("sam-3.1", ModelKind::Other),
        ("muse-spark-1.3", ModelKind::Chat),
        ("new-reasoning-model", ModelKind::Reasoning),
        ("llama-vision-model", ModelKind::VisionCapable),
        ("unknown-chat-2027", ModelKind::Chat),
    ] {
        let metadata = model_metadata("unknown", &ids(&[id]), false);
        assert_eq!(metadata[0].kind, kind, "{id}");
        assert!(!metadata[0].recommended, "{id}");
        assert_eq!(
            model_metadata("unknown", &ids(&[id]), true).len(),
            usize::from(kind.is_chat()),
            "{id}"
        );
    }
}

#[test]
fn names_are_readable_and_free_notes_only_come_from_live_openrouter_ids() {
    let models = model_metadata("openrouter", &ids(&["vendor/new-chat:free"]), true);
    assert_eq!(models[0].display_name, "Vendor New Chat Free");
    assert_eq!(models[0].note.as_deref(), Some("free"));
    assert!(!models[0].recommended);
    assert_eq!(model_metadata("openrouter", &[], true), Vec::new());
    assert_eq!(
        model_metadata("other", &ids(&["vendor/new-chat:free"]), true)[0].note,
        None
    );
    assert_eq!(humanize("gemini-2.5-flash-lite"), "Gemini 2.5 Flash Lite");
    assert_eq!(humanize("openai/gpt-6-luna"), "OpenAI GPT 6 Luna");
}

#[test]
fn invalid_listed_ids_are_omitted_without_becoming_an_execution_allowlist() {
    assert_eq!(
        model_metadata(
            "other",
            &ids(&["", "a\nb", "a\u{202e}b", "a?api_key=value"]),
            false
        ),
        Vec::new()
    );
    assert_eq!(
        model_metadata("other", &["a".repeat(257)], false),
        Vec::new()
    );
    let id = "my-custom-model-not-in-the-table";
    let entry = crate::ProviderEntry {
        id: "custom".into(),
        kind: crate::ProviderKind::OpenAiCompatible,
        base_url: "http://127.0.0.1:1/v1".into(),
        auth: crate::Auth::None,
        capabilities: crate::ModelCapabilities {
            tools: true,
            vision: false,
            streaming: true,
            json_mode: false,
        },
        compat_flags: crate::CompatFlags::default(),
        models_endpoint: None,
        models: Vec::new(),
    };
    assert_eq!(entry.caps_for(id), entry.capabilities);
    assert_eq!(model_metadata("custom", &ids(&[id]), true)[0].id, id);
}
