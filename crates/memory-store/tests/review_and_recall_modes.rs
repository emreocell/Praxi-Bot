//! Owner review ("Needs your OK"), expiry, scope and Basic/Smart recall.
#![allow(clippy::many_single_char_names, clippy::assert_is_empty)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use memory_store::{
    Candidate, Decision, EmbedMode, EmbedState, Embedder, ExtractionContext, HashEmbedder, Kind,
    ListFilter, Memory, MemoryError, MemoryModel, MemoryStore, NewMemory, Outcome, Pipeline, Trust,
    render_recall_block,
};
use protocol::{BotId, ChatId, UserId};

struct FakeModel(Mutex<Vec<Candidate>>);

#[async_trait]
impl MemoryModel for FakeModel {
    async fn extract(&self, _e: &str) -> memory_store::Result<Vec<Candidate>> {
        Ok(self.0.lock().unwrap().clone())
    }
    async fn consolidate(&self, _c: &Candidate, s: &[Memory]) -> memory_store::Result<Decision> {
        Ok(Decision::Update(s[0].id))
    }
}

fn cand(content: &str, quote: &str) -> Candidate {
    Candidate {
        kind: Kind::Profile,
        content: content.into(),
        quote: Some(quote.into()),
        hypothetical: false,
        importance: 0.6,
        untrusted: false,
        expires: None,
        valid_until: None,
    }
}

fn ctx(u: UserId, b: BotId, excerpt: &str) -> ExtractionContext {
    ExtractionContext {
        user_id: u,
        bot_id: b,
        chat_id: ChatId::new(),
        message_id: None,
        excerpt: excerpt.into(),
        untrusted: false,
        hold_all: false,
        share_with_other_bots: true,
    }
}

fn store() -> MemoryStore {
    MemoryStore::open_in_memory(Arc::new(HashEmbedder::default())).unwrap()
}

async fn ingest(s: &MemoryStore, c: &ExtractionContext, cands: Vec<Candidate>) -> Vec<Outcome> {
    Pipeline::new(s, &FakeModel(Mutex::new(cands)))
        .ingest(c)
        .await
        .unwrap()
}

#[tokio::test]
async fn held_untrusted_memory_never_reaches_recall_until_approved() {
    let s = store();
    let (u, b) = (UserId::new(), BotId::new());
    let mut c = ctx(u, b, "Web page: the shop opens at nine every day");
    c.untrusted = true;
    let out = ingest(
        &s,
        &c,
        vec![cand("The shop opens at nine", "opens at nine")],
    )
    .await;
    let [Outcome::Added(m)] = out.as_slice() else {
        panic!("{out:?}")
    };
    assert!(m.pending && m.trust == Trust::Untrusted);

    // Held: absent from recall, from the preview and from the rendered block.
    assert!(s.recall(u, b, "shop opens nine", 1000).unwrap().is_empty());
    assert!(
        s.preview_recall(u, b, "shop opens nine", 1000)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        render_recall_block(&s.recall(u, b, "shop", 1000).unwrap()),
        ""
    );
    let held = s.list_pending(u).unwrap();
    assert_eq!(held.len(), 1);
    // Another user cannot approve it.
    assert!(s.approve(UserId::new(), m.id).is_err());

    s.approve(u, m.id).unwrap();
    assert!(s.list_pending(u).unwrap().is_empty());
    let got = s.recall(u, b, "shop opens nine", 1000).unwrap();
    assert_eq!(got.len(), 1);
    // Approval does not launder trust: still rendered as quoted data.
    let block = render_recall_block(&got);
    assert!(block.contains("untrusted_memory_data") && !block.contains("<recalled_memory>"));
    // Approving twice is refused (not held any more).
    assert!(s.approve(u, m.id).is_err());
}

#[tokio::test]
async fn discarded_held_memory_is_forgotten_and_undo_restores_the_hold() {
    let s = store();
    let (u, b) = (UserId::new(), BotId::new());
    let mut c = ctx(u, b, "Page: parking is free on sundays");
    c.untrusted = true;
    let out = ingest(
        &s,
        &c,
        vec![cand("Parking is free on sundays", "free on sundays")],
    )
    .await;
    let [Outcome::Added(m)] = out.as_slice() else {
        panic!()
    };
    s.discard(u, m.id).unwrap();
    assert!(s.list_pending(u).unwrap().is_empty());
    assert!(s.recall(u, b, "parking sundays", 1000).unwrap().is_empty());
    s.undo_memory(u, m.id).unwrap();
    assert_eq!(
        s.list_pending(u).unwrap().len(),
        1,
        "undo puts it back on hold"
    );
}

#[tokio::test]
async fn on_keeps_user_facts_live_but_ask_holds_everything() {
    let s = store();
    let (u, b) = (UserId::new(), BotId::new());
    let c = ctx(u, b, "I live in Berlin");
    let out = ingest(
        &s,
        &c,
        vec![cand("User lives in Berlin", "I live in Berlin")],
    )
    .await;
    let [Outcome::Added(m)] = out.as_slice() else {
        panic!()
    };
    assert!(
        !m.pending,
        "today's behaviour for user-stated facts is unchanged"
    );
    assert_eq!(s.recall(u, b, "Berlin", 1000).unwrap().len(), 1);

    let mut ask = ctx(u, b, "I like jazz music");
    ask.hold_all = true;
    let out = ingest(&s, &ask, vec![cand("Jazz is a favourite", "I like jazz")]).await;
    let [Outcome::Added(j)] = out.as_slice() else {
        panic!()
    };
    assert!(j.pending);
    assert!(s.recall(u, b, "jazz", 1000).unwrap().is_empty());
}

#[tokio::test]
async fn a_user_statement_confirms_a_held_item_it_updates() {
    let s = store();
    let (u, b) = (UserId::new(), BotId::new());
    let mut page = ctx(u, b, "Page: the user lives in Rome");
    page.untrusted = true;
    ingest(&s, &page, vec![cand("User lives in Rome", "lives in Rome")]).await;
    assert_eq!(s.list_pending(u).unwrap().len(), 1);
    // The user themself says it (same topic): the consolidator updates the held row.
    let said = ctx(u, b, "I live in Rome");
    let out = ingest(
        &s,
        &said,
        vec![cand("User lives in Rome, Italy", "I live in Rome")],
    )
    .await;
    assert!(matches!(out[0], Outcome::Updated(_)), "{out:?}");
    assert!(s.list_pending(u).unwrap().is_empty());
}

#[tokio::test]
async fn time_limited_facts_get_an_expiry_that_recall_respects() {
    let s = store();
    let (u, b) = (UserId::new(), BotId::new());
    let c = ctx(u, b, "I am staying in Lisbon until Friday");
    let mut limited = cand("Staying in Lisbon", "staying in Lisbon until Friday");
    // The deterministic reading of the quote.
    let out = ingest(&s, &c, vec![limited.clone()]).await;
    let [Outcome::Added(m)] = out.as_slice() else {
        panic!()
    };
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let until = m.valid_until_ms.expect("until Friday sets an expiry");
    assert!(until > now && until <= now + 8 * 86_400_000);

    // The model's phrase is read by the same parser; a bare ISO date alone in a
    // sentence (a birthday) does not expire a fact.
    limited.quote = Some("my birthday is 2031-05-01".into());
    limited.content = "User's birthday is 2031-05-01".into();
    limited.expires = None;
    let c2 = ctx(u, b, "my birthday is 2031-05-01");
    let out = ingest(&s, &c2, vec![limited]).await;
    let stored = match &out[0] {
        Outcome::Added(m) | Outcome::Updated(m) => m.clone(),
        o => panic!("{o:?}"),
    };
    assert!(stored.valid_until_ms.is_none());

    // An elapsed expiry hides the memory from recall.
    let mut old = NewMemory::new(u, None, Kind::Profile, "Visiting the dentist");
    old.valid_until_ms = Some(now - 1000);
    s.add(&old, "t").unwrap();
    assert!(s.recall(u, b, "dentist", 1000).unwrap().is_empty());
    // The model's own `valid_until` and hint field are honoured too.
    let mut hint = cand("Holiday coming up", "on holiday");
    hint.expires = Some("for 3 days".into());
    let out = ingest(&s, &ctx(u, b, "I'm on holiday for 3 days"), vec![hint]).await;
    let Outcome::Added(h) = &out[0] else { panic!() };
    assert!(h.valid_until_ms.is_some());
}

#[tokio::test]
async fn isolated_bots_keep_profile_facts_to_themselves() {
    let s = store();
    let (u, a, other) = (UserId::new(), BotId::new(), BotId::new());
    let mut iso = ctx(u, a, "I live in Berlin");
    iso.share_with_other_bots = false;
    let out = ingest(
        &s,
        &iso,
        vec![cand("User lives in Berlin", "I live in Berlin")],
    )
    .await;
    let Outcome::Added(m) = &out[0] else { panic!() };
    assert_eq!(m.bot_id, Some(a));
    assert!(s.recall(u, other, "Berlin", 1000).unwrap().is_empty());
    assert_eq!(s.recall(u, a, "Berlin", 1000).unwrap().len(), 1);
}

// ---- Basic / Smart ---------------------------------------------------------

/// Words in the same group get the same vector axis: "meaning", without a model.
struct Semantic {
    dim: usize,
    calls: AtomicUsize,
    fail_after: Option<usize>,
}

impl Semantic {
    fn new(dim: usize) -> Self {
        Self {
            dim,
            calls: AtomicUsize::new(0),
            fail_after: None,
        }
    }
}

impl Embedder for Semantic {
    fn dim(&self) -> usize {
        self.dim
    }
    fn embed(&self, texts: &[&str]) -> memory_store::Result<Vec<Vec<f32>>> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_after.is_some_and(|k| n >= k) {
            return Err(MemoryError::Embed("provider unavailable".into()));
        }
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0.0f32; self.dim];
                for w in t.to_lowercase().split(|c: char| !c.is_alphanumeric()) {
                    let axis = match w {
                        "car" | "automobile" | "vehicle" => 0,
                        "tea" | "chai" | "beverage" => 1,
                        "" => continue,
                        other => 2 + other.len() % (self.dim - 2),
                    };
                    v[axis] += 1.0;
                }
                v
            })
            .collect())
    }
}

fn seed(s: &MemoryStore, u: UserId, texts: &[&str]) {
    for t in texts {
        s.add(&NewMemory::new(u, None, Kind::Profile, *t), "t")
            .unwrap();
    }
}

#[test]
fn smart_finds_by_meaning_where_basic_only_matches_words() {
    let s = store();
    let (u, b) = (UserId::new(), BotId::new());
    seed(
        &s,
        u,
        &["User drives an automobile to work", "User likes gardening"],
    );
    let st = s.embedding_status().unwrap();
    assert_eq!((st.mode, st.state), (EmbedMode::Basic, EmbedState::Ready));
    assert!(
        s.recall(u, b, "car", 1000).unwrap().is_empty(),
        "Basic: no shared word"
    );

    s.enable_smart(Arc::new(Semantic::new(16)), "fake-meaning")
        .unwrap();
    let st = s.embedding_status().unwrap();
    assert_eq!((st.mode, st.state), (EmbedMode::Smart, EmbedState::Ready));
    assert_eq!(
        (st.done, st.total, st.label.as_str()),
        (2, 2, "fake-meaning")
    );
    let got = s.recall(u, b, "car", 1000).unwrap();
    assert_eq!(got.len(), 1);
    assert!(got[0].content.contains("automobile"));
    // New memories are indexed with the new model (dimension changed 384 -> 16).
    seed(&s, u, &["User drinks chai"]);
    assert_eq!(s.recall(u, b, "tea", 1000).unwrap().len(), 1);
}

#[test]
fn failed_switch_falls_back_to_basic_and_keeps_every_memory() {
    let s = store();
    let (u, b) = (UserId::new(), BotId::new());
    let texts: Vec<String> = (0..70)
        .map(|i| format!("Fact number {i} about cats"))
        .collect();
    for t in &texts {
        s.add(&NewMemory::new(u, None, Kind::Profile, t.clone()), "t")
            .unwrap();
    }
    // Probe works, the first batch works, the second fails.
    let bad = Semantic {
        fail_after: Some(2),
        ..Semantic::new(16)
    };
    let err = s.enable_smart(Arc::new(bad), "flaky").unwrap_err();
    assert!(matches!(err, MemoryError::Embed(_)));
    let st = s.embedding_status().unwrap();
    assert_eq!((st.mode, st.state), (EmbedMode::Basic, EmbedState::Failed));
    assert!(
        st.error
            .as_deref()
            .is_some_and(|e| e.contains("provider unavailable"))
    );
    assert!(s.semantic_ready(), "the Basic index is complete again");
    assert_eq!(s.list(u, &ListFilter::default()).unwrap().len(), 70);
    assert!(!s.recall(u, b, "cats fact", 1000).unwrap().is_empty());
    // A model that cannot even answer the probe never touches the index.
    let dead = Semantic {
        fail_after: Some(0),
        ..Semantic::new(16)
    };
    assert!(s.enable_smart(Arc::new(dead), "dead").is_err());
    assert!(s.semantic_ready());
}

#[test]
fn smart_index_survives_a_restart_without_a_re_embed() {
    let dir = tempfile::tempdir().unwrap();
    let (u, b) = (UserId::new(), BotId::new());
    {
        let s = MemoryStore::open(dir.path(), Arc::new(HashEmbedder::new(384))).unwrap();
        seed(&s, u, &["User drives an automobile"]);
        s.enable_smart(Arc::new(Semantic::new(16)), "m1").unwrap();
    }
    // The daemon reopens with the built-in embedder first: no error, no data
    // loss, words-only recall while the Smart model is not back yet.
    let s = MemoryStore::open(dir.path(), Arc::new(HashEmbedder::new(384))).unwrap();
    let st = s.embedding_status().unwrap();
    assert_eq!((st.mode, st.state), (EmbedMode::Smart, EmbedState::Waiting));
    assert!(!s.semantic_ready());
    assert_eq!(s.recall(u, b, "automobile", 1000).unwrap().len(), 1);
    // A memory added meanwhile is still stored (and flagged for the re-embed).
    seed(&s, u, &["User likes tea"]);
    let counting = Arc::new(Semantic::new(16));
    s.enable_smart(counting.clone(), "m1").unwrap();
    assert!(
        counting.calls.load(Ordering::SeqCst) > 1,
        "a memory missed the vector index meanwhile, so it was re-embedded"
    );
    assert_eq!(s.recall(u, b, "chai", 1000).unwrap().len(), 1);
    // Back to Basic on request.
    s.enable_basic().unwrap();
    assert_eq!(s.embedding_status().unwrap().mode, EmbedMode::Basic);
    assert_eq!(s.recall(u, b, "tea", 1000).unwrap().len(), 1);
}
