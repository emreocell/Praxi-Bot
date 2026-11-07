//! Acceptance tests: fake embedder + fake model.
#![allow(clippy::many_single_char_names, clippy::assert_is_empty)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use memory_store::{
    Candidate, Decision, Embedder, ExtractionContext, HashEmbedder, JournalOp, Kind, ListFilter,
    MemoryError, MemoryModel, MemoryStore, NewMemory, Outcome, Pipeline, RejectReason, Trust,
    render_recall_block,
};
use protocol::{BotId, ChatId, UserId};
use uuid::Uuid;

fn store() -> MemoryStore {
    MemoryStore::open_in_memory(Arc::new(HashEmbedder::default())).unwrap()
}

#[derive(Default)]
struct FakeModel {
    candidates: Mutex<Vec<Candidate>>,
    decision: Mutex<Option<Decision>>,
    consolidate_calls: Mutex<usize>,
}

impl FakeModel {
    fn with(candidates: Vec<Candidate>) -> Self {
        Self {
            candidates: Mutex::new(candidates),
            ..Self::default()
        }
    }
}

#[async_trait]
impl MemoryModel for FakeModel {
    async fn extract(&self, _excerpt: &str) -> memory_store::Result<Vec<Candidate>> {
        Ok(self.candidates.lock().unwrap().clone())
    }
    async fn consolidate(
        &self,
        _c: &Candidate,
        similar: &[memory_store::Memory],
    ) -> memory_store::Result<Decision> {
        *self.consolidate_calls.lock().unwrap() += 1;
        Ok(self.decision.lock().unwrap().unwrap_or_else(|| {
            // Default: first similar memory is the one to update.
            Decision::Update(similar[0].id)
        }))
    }
}

fn cand(content: &str, quote: Option<&str>) -> Candidate {
    Candidate {
        kind: Kind::Profile,
        content: content.into(),
        quote: quote.map(Into::into),
        hypothetical: false,
        importance: 0.6,
        untrusted: false,
    }
}

fn ctx(user: UserId, bot: BotId, excerpt: &str) -> ExtractionContext {
    ExtractionContext {
        user_id: user,
        bot_id: bot,
        chat_id: ChatId::new(),
        message_id: Some(Uuid::now_v7()),
        excerpt: excerpt.into(),
        untrusted: false,
    }
}

#[tokio::test]
async fn extraction_adds() {
    let s = store();
    let (u, b) = (UserId::new(), BotId::new());
    let model = FakeModel::with(vec![cand("User lives in Berlin", Some("I live in Berlin"))]);
    let out = Pipeline::new(&s, &model)
        .ingest(&ctx(u, b, "Hi! I live in Berlin and work remotely."))
        .await
        .unwrap();
    let [Outcome::Added(m)] = out.as_slice() else {
        panic!("{out:?}")
    };
    assert_eq!(m.trust, Trust::User);
    assert_eq!(m.source_quote.as_deref(), Some("I live in Berlin"));
    assert!(m.bot_id.is_none(), "profile memories are shared");
    assert_eq!(s.list(u, &ListFilter::default()).unwrap().len(), 1);
    assert_eq!(s.journal(u, 10).unwrap()[0].op, JournalOp::Add);
    // First memory: no similar ones, so the model is not asked to consolidate.
    assert_eq!(*model.consolidate_calls.lock().unwrap(), 0);
}

#[tokio::test]
async fn duplicate_updates_instead_of_adding() {
    let s = store();
    let (u, b) = (UserId::new(), BotId::new());
    let first = FakeModel::with(vec![cand("User lives in Berlin", Some("I live in Berlin"))]);
    Pipeline::new(&s, &first)
        .ingest(&ctx(u, b, "I live in Berlin"))
        .await
        .unwrap();
    let again = FakeModel::with(vec![cand(
        "User lives in Berlin, Germany",
        Some("I live in Berlin, Germany"),
    )]);
    let out = Pipeline::new(&s, &again)
        .ingest(&ctx(u, b, "I live in Berlin, Germany"))
        .await
        .unwrap();
    assert!(matches!(out[0], Outcome::Updated(_)), "{out:?}");
    let all = s.list(u, &ListFilter::default()).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].content, "User lives in Berlin, Germany");
}

#[tokio::test]
async fn contradiction_updates_with_journal() {
    let s = store();
    let (u, b) = (UserId::new(), BotId::new());
    let first = FakeModel::with(vec![cand("User lives in Berlin", Some("I live in Berlin"))]);
    Pipeline::new(&s, &first)
        .ingest(&ctx(u, b, "I live in Berlin"))
        .await
        .unwrap();
    let second = FakeModel::with(vec![cand(
        "User lives in Paris",
        Some("I moved to Paris, I live in Paris now"),
    )]);
    let out = Pipeline::new(&s, &second)
        .ingest(&ctx(u, b, "Update: I moved to Paris, I live in Paris now"))
        .await
        .unwrap();
    let [Outcome::Updated(m)] = out.as_slice() else {
        panic!("{out:?}")
    };
    assert_eq!(m.content, "User lives in Paris");
    let j = s.journal(u, 10).unwrap();
    assert_eq!(j[0].op, JournalOp::Update);
    assert_eq!(
        j[0].before.as_ref().unwrap().content,
        "User lives in Berlin"
    );
    assert_eq!(j[0].after.as_ref().unwrap().content, "User lives in Paris");
}

#[tokio::test]
async fn questions_and_hypotheticals_not_stored() {
    let s = store();
    let (u, b) = (UserId::new(), BotId::new());
    let mut flagged = cand("User owns a boat", Some("I own a boat"));
    flagged.hypothetical = true;
    let model = FakeModel::with(vec![
        cand("User lives in Berlin", Some("Do I live in Berlin?")),
        cand("User moves to Paris", Some("What if I moved to Paris")),
        flagged,
    ]);
    let out = Pipeline::new(&s, &model)
        .ingest(&ctx(
            u,
            b,
            "Do I live in Berlin? What if I moved to Paris. I own a boat",
        ))
        .await
        .unwrap();
    assert_eq!(out, vec![Outcome::Rejected(RejectReason::Question); 3]);
    assert!(s.list(u, &ListFilter::default()).unwrap().is_empty());
}

#[tokio::test]
async fn quote_required_and_must_come_from_source() {
    let s = store();
    let (u, b) = (UserId::new(), BotId::new());
    let model = FakeModel::with(vec![
        cand("User likes tea", None),
        cand("User likes tea", Some("   ")),
        cand("User likes coffee", Some("I adore coffee")),
    ]);
    let out = Pipeline::new(&s, &model)
        .ingest(&ctx(u, b, "I like tea"))
        .await
        .unwrap();
    assert_eq!(
        out,
        vec![
            Outcome::Rejected(RejectReason::NoQuote),
            Outcome::Rejected(RejectReason::NoQuote),
            Outcome::Rejected(RejectReason::QuoteNotInSource),
        ]
    );
    assert!(s.list(u, &ListFilter::default()).unwrap().is_empty());
}

#[tokio::test]
async fn secrets_rejected() {
    let s = store();
    let (u, b) = (UserId::new(), BotId::new());
    let model = FakeModel::with(vec![cand(
        "User's key is sk-abcdefghijklmnop1234",
        Some("my key is sk-abcdefghijklmnop1234"),
    )]);
    let out = Pipeline::new(&s, &model)
        .ingest(&ctx(u, b, "my key is sk-abcdefghijklmnop1234"))
        .await
        .unwrap();
    assert_eq!(out, vec![Outcome::Rejected(RejectReason::Secret)]);
    // The store itself refuses too.
    let direct = s.add(
        &NewMemory::new(u, None, Kind::Profile, "password: hunter2"),
        "t",
    );
    assert!(matches!(direct, Err(MemoryError::Invalid(_))));
}

#[tokio::test]
async fn untrusted_flagged_and_cannot_overwrite_user_memories() {
    let s = store();
    let (u, b) = (UserId::new(), BotId::new());
    let user_fact = FakeModel::with(vec![cand("User lives in Berlin", Some("I live in Berlin"))]);
    Pipeline::new(&s, &user_fact)
        .ingest(&ctx(u, b, "I live in Berlin"))
        .await
        .unwrap();

    // A web page says something about the same topic: consolidator wants to update.
    let page = FakeModel::with(vec![cand(
        "User lives in Mars",
        Some("the user lives in Mars"),
    )]);
    let mut c = ctx(u, b, "Web page text: the user lives in Mars");
    c.untrusted = true;
    let out = Pipeline::new(&s, &page).ingest(&c).await.unwrap();
    assert_eq!(out, vec![Outcome::Skipped]);
    assert_eq!(
        s.list(u, &ListFilter::default()).unwrap()[0].content,
        "User lives in Berlin"
    );

    // Unrelated untrusted fact is stored, flagged untrusted.
    let page2 = FakeModel::with(vec![cand("Store opens at nine", Some("opens at nine"))]);
    let mut c2 = ctx(u, b, "Shop page: opens at nine");
    c2.untrusted = true;
    let out = Pipeline::new(&s, &page2).ingest(&c2).await.unwrap();
    let [Outcome::Added(m)] = out.as_slice() else {
        panic!("{out:?}")
    };
    assert_eq!(m.trust, Trust::Untrusted);
    let block = render_recall_block(std::slice::from_ref(m));
    assert!(block.contains("untrusted_memory_data") && !block.contains("<recalled_memory>"));
}

#[test]
fn recall_fuses_lexical_and_vector_hits() {
    let s = store();
    let (u, b) = (UserId::new(), BotId::new());
    let add = |t: &str| {
        s.add(&NewMemory::new(u, None, Kind::Profile, t), "t")
            .unwrap()
    };
    let both = add("User drinks green tea every morning");
    let _weak = add("User has a garden with tomatoes");
    let _other = add("User prefers dark mode in editors");
    let got = s.recall(u, b, "green tea", 1000).unwrap();
    assert_eq!(got[0].id, both.id);
    assert!(
        got.iter()
            .all(|m| m.content != "User prefers dark mode in editors"),
        "unrelated memory must not be recalled: {got:?}"
    );
}

#[test]
fn recall_respects_bot_and_user_scope_and_deletion() {
    let s = store();
    let (u, other_user) = (UserId::new(), UserId::new());
    let (b1, b2) = (BotId::new(), BotId::new());
    s.add(
        &NewMemory::new(u, Some(b1), Kind::Bot, "Bot one likes pirate jokes"),
        "t",
    )
    .unwrap();
    let shared = s
        .add(
            &NewMemory::new(u, None, Kind::Profile, "User likes pirate movies"),
            "t",
        )
        .unwrap();
    s.add(
        &NewMemory::new(
            other_user,
            None,
            Kind::Profile,
            "Stranger likes pirate food",
        ),
        "t",
    )
    .unwrap();

    let for_b2 = s.recall(u, b2, "pirate", 1000).unwrap();
    assert_eq!(for_b2.len(), 1);
    assert_eq!(for_b2[0].id, shared.id);
    assert_eq!(s.recall(u, b1, "pirate", 1000).unwrap().len(), 2);

    s.delete(u, shared.id, "t").unwrap();
    assert_eq!(s.recall(u, b2, "pirate", 1000).unwrap().len(), 0);
    // Other users cannot delete it.
    assert!(matches!(
        s.delete(other_user, shared.id, "t"),
        Err(MemoryError::NotFound(_))
    ));
}

#[test]
fn budget_cuts_results() {
    let s = store();
    let (u, b) = (UserId::new(), BotId::new());
    for i in 0..5 {
        s.add(
            &NewMemory::new(
                u,
                None,
                Kind::Profile,
                format!("Fact {i} about pirate ships and sailing the sea"),
            ),
            "t",
        )
        .unwrap();
    }
    assert_eq!(s.recall(u, b, "pirate", 10_000).unwrap().len(), 5);
    // Each fact costs ~ 11 + 4 tokens.
    let cut = s.recall(u, b, "pirate", 35).unwrap();
    assert_eq!(cut.len(), 2);
    assert!(s.recall(u, b, "pirate", 3).unwrap().is_empty());
}

#[test]
fn undo_restores() {
    let s = store();
    let (u, b) = (UserId::new(), BotId::new());
    let m = s
        .add(
            &NewMemory::new(u, None, Kind::Profile, "User lives in Berlin"),
            "t",
        )
        .unwrap();

    // update -> undo
    s.edit(u, m.id, "User lives in Paris").unwrap();
    let j = s.journal(u, 10).unwrap();
    let restored = s.undo(u, j[0].id).unwrap();
    assert_eq!(restored.content, "User lives in Berlin");
    assert_eq!(
        s.get(m.id).unwrap().unwrap().content,
        "User lives in Berlin"
    );
    assert_eq!(
        s.recall(u, b, "Berlin", 100).unwrap().len(),
        1,
        "re-indexed"
    );
    assert!(s.recall(u, b, "Paris", 100).unwrap().is_empty());
    assert!(
        matches!(s.undo(u, j[0].id), Err(MemoryError::Invalid(_))),
        "double undo"
    );

    // delete -> undo
    s.delete(u, m.id, "t").unwrap();
    assert!(s.recall(u, b, "Berlin", 100).unwrap().is_empty());
    let del = s.journal(u, 1).unwrap().remove(0);
    assert_eq!(del.op, JournalOp::Delete);
    s.undo(u, del.id).unwrap();
    assert_eq!(s.recall(u, b, "Berlin", 100).unwrap().len(), 1);

    // Undo is stack-like: the update/delete entries are undone, so the add is now newest.
    s.edit(u, m.id, "User lives in Rome").unwrap();
    let add_entry = s
        .journal(u, 10)
        .unwrap()
        .into_iter()
        .find(|e| e.op == JournalOp::Add)
        .unwrap();
    assert!(
        matches!(s.undo(u, add_entry.id), Err(MemoryError::Invalid(_))),
        "newer edit blocks it"
    );

    // add -> undo
    let fresh = s
        .add(&NewMemory::new(u, None, Kind::Profile, "Likes jazz"), "t")
        .unwrap();
    let e = s.journal(u, 1).unwrap().remove(0);
    s.undo(u, e.id).unwrap();
    assert!(s.get(fresh.id).unwrap().unwrap().deleted);
    // another user cannot undo
    assert!(matches!(
        s.undo(UserId::new(), e.id),
        Err(MemoryError::NotFound(_))
    ));
}

#[test]
fn corrections_become_bot_scoped_lessons() {
    let s = store();
    let (u, b1, b2) = (UserId::new(), BotId::new(), BotId::new());
    let l = s
        .record_correction(
            u,
            b1,
            ChatId::new(),
            "answer in German",
            "always answer in English",
        )
        .unwrap();
    assert_eq!(l.kind, Kind::Lesson);
    assert_eq!(l.bot_id, Some(b1));
    assert_eq!(s.recall(u, b1, "answer English", 100).unwrap().len(), 1);
    assert!(s.recall(u, b2, "answer English", 100).unwrap().is_empty());
    assert!(s.record_correction(u, b1, ChatId::new(), "", "x").is_err());
}

#[test]
fn export_contains_memories_and_journal() {
    let s = store();
    let u = UserId::new();
    let m = s
        .add(&NewMemory::new(u, None, Kind::Profile, "Likes jazz"), "t")
        .unwrap();
    s.delete(u, m.id, "t").unwrap();
    let v = s.export_json(u).unwrap();
    assert_eq!(v["memories"].as_array().unwrap().len(), 1);
    assert_eq!(v["memories"][0]["deleted"], true);
    assert_eq!(v["journal"].as_array().unwrap().len(), 2);
    assert!(s.list(u, &ListFilter::default()).unwrap().is_empty());
}

#[test]
fn session_search_over_messages() {
    let dir = tempfile::tempdir().unwrap();
    let db = storage::Storage::open_in_memory(dir.path()).unwrap();
    let user = storage::User {
        id: UserId::new(),
        name: "u".into(),
        created_at_ms: 1,
    };
    db.insert_user(&user).unwrap();
    let bot = storage::Bot {
        id: BotId::new(),
        user_id: user.id,
        name: "b".into(),
        config: serde_json::json!({}),
        created_at_ms: 1,
    };
    db.insert_bot(&bot).unwrap();
    let chat = storage::Chat {
        id: ChatId::new(),
        user_id: user.id,
        bot_id: bot.id,
        title: "t".into(),
        created_at_ms: 1,
    };
    db.insert_chat(&chat).unwrap();
    for (i, (role, text)) in [
        ("user", "Remind me about the quarterly invoice"),
        ("assistant", "Sure, invoice reminder noted"),
        ("user", "Unrelated chatter about weather"),
        ("tool", "invoice secret tool output"),
    ]
    .into_iter()
    .enumerate()
    {
        db.insert_message(&storage::Message {
            id: Uuid::now_v7(),
            user_id: user.id,
            chat_id: chat.id,
            run_id: None,
            role: role.into(),
            content: text.into(),
            created_at_ms: i64::try_from(i).unwrap(),
        })
        .unwrap();
    }
    let s = store();
    assert_eq!(s.index_chat(&db, chat.id).unwrap(), 3);
    assert_eq!(s.index_chat(&db, chat.id).unwrap(), 0, "idempotent");
    let hits = s.search_sessions(user.id, "invoice", 10).unwrap();
    assert_eq!(hits.len(), 2);
    assert!(hits.iter().all(|h| h.chat_id == chat.id));
    assert!(
        s.search_sessions(UserId::new(), "invoice", 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn persists_on_disk_and_checks_dimension() {
    let dir = tempfile::tempdir().unwrap();
    let u = UserId::new();
    let b = BotId::new();
    {
        let s = MemoryStore::open(dir.path(), Arc::new(HashEmbedder::new(64))).unwrap();
        s.add(&NewMemory::new(u, None, Kind::Profile, "Likes jazz"), "t")
            .unwrap();
    }
    let s = MemoryStore::open(dir.path(), Arc::new(HashEmbedder::new(64))).unwrap();
    assert_eq!(s.recall(u, b, "jazz", 100).unwrap().len(), 1);
    drop(s);
    let err = MemoryStore::open(dir.path(), Arc::new(HashEmbedder::new(32))).unwrap_err();
    assert!(matches!(
        err,
        MemoryError::DimensionMismatch {
            db: 64,
            embedder: 32
        }
    ));
    assert_eq!(HashEmbedder::new(64).dim(), 64);
}

#[test]
fn prompts_ship_as_files() {
    assert!(memory_store::prompts::render_extract("hello").contains("hello"));
    assert!(memory_store::prompts::render_consolidate("c", "s").contains("NEW CANDIDATE"));
}
