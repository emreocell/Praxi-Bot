//! Folder documents end to end: extract, index, search, cite, re-index.

use lopdf::content::{Content, Operation};
use lopdf::{Document, Object, Stream, dictionary};
use memory_store::knowledge::source_adapters::SelectedSource;
use memory_store::knowledge::{
    KnowledgeStore, NewSource, Scope, SearchRequest, Source, SourceKind,
};
use protocol::{BotId, UserId};
use std::path::Path;

/// A PDF with one text line per entry; each entry is one page.
fn pdf(pages: &[&[&str]]) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let parent_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
    });
    let resources_id = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font_id } });
    let mut kids = Vec::new();
    for lines in pages {
        let mut ops = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 12.into()]),
            Operation::new("TL", vec![14.into()]),
            Operation::new("Td", vec![50.into(), 700.into()]),
        ];
        for line in *lines {
            ops.push(Operation::new("Tj", vec![Object::string_literal(*line)]));
            ops.push(Operation::new("T*", vec![]));
        }
        ops.push(Operation::new("ET", vec![]));
        let content = Content { operations: ops };
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let leaf_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => parent_id, "Contents" => content_id,
        });
        kids.push(leaf_id.into());
    }
    let count = i64::try_from(kids.len()).unwrap();
    doc.objects.insert(
        parent_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => kids, "Count" => count, "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        }),
    );
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => parent_id });
    doc.trailer.set("Root", catalog_id);
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}

fn store_with_folder(folder: &Path) -> (KnowledgeStore, Source) {
    let store = KnowledgeStore::open_in_memory(Scope {
        user_id: UserId::new(),
        bot_id: BotId::new(),
    })
    .unwrap();
    let source = store
        .add_source(NewSource::new(
            SourceKind::Folder,
            "Papers",
            folder.display().to_string(),
        ))
        .unwrap();
    (store, source)
}

/// Index every selected file; returns (changed, unchanged) counts and the new revision.
fn index_all(store: &KnowledgeStore, source: &Source, folder: &Path) -> (usize, usize, u64) {
    let selected = SelectedSource::open(folder, true).unwrap();
    let mut revision = store.source(source.id).unwrap().unwrap().revision;
    let (mut changed, mut unchanged) = (0, 0);
    for name in selected.files() {
        let outcome = store
            .index_document(source.id, revision, &selected.read(name).unwrap())
            .unwrap();
        revision = outcome.source_revision;
        if outcome.unchanged {
            unchanged += 1;
        } else {
            changed += 1;
        }
    }
    (changed, unchanged, revision)
}

#[test]
fn a_pdf_passage_is_cited_with_its_file_and_page() {
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(
        folder.path().join("lease.pdf"),
        pdf(&[
            &["Rental agreement between the parties."],
            &["The security deposit is 1800 euro and is returned within thirty days."],
            &["Signatures."],
        ]),
    )
    .unwrap();
    let (store, source) = store_with_folder(folder.path());
    index_all(&store, &source, folder.path());
    let hits = store
        .search(&SearchRequest::new("security deposit amount"))
        .unwrap()
        .hits;
    assert_eq!(hits.len(), 1, "{hits:?}");
    let hit = &hits[0];
    assert_eq!(hit.page, Some(2));
    assert_eq!(
        hit.path.as_deref(),
        Some(
            folder
                .path()
                .join("lease.pdf")
                .display()
                .to_string()
                .as_str()
        )
    );
    let citation = hit.citation.as_deref().unwrap();
    assert!(
        citation.starts_with("[lease.pdf, p. 2](praxi-doc://"),
        "{citation}"
    );
    assert!(citation.ends_with("&page=2)"), "{citation}");
    // Another source kind never gets a file citation.
    let note = store
        .add_source(NewSource::new(SourceKind::Note, "Note", "note:1"))
        .unwrap();
    store
        .index_document(
            note.id,
            note.revision,
            &memory_store::knowledge::DocumentInput {
                locator: "note:1".into(),
                title: "Note".into(),
                format: memory_store::knowledge::DocumentFormat::Markdown,
                content: "The security deposit amount is in my head.".into(),
                mtime_ms: None,
            },
        )
        .unwrap();
    let both = store
        .search(&SearchRequest::new("security deposit amount"))
        .unwrap()
        .hits;
    assert_eq!(both.len(), 2);
    assert!(both.iter().any(|h| h.citation.is_none()));
    // `files_only` is what `documents_search` uses: only the PDF remains.
    let mut files = SearchRequest::new("security deposit amount");
    files.files_only = true;
    let only = store.search(&files).unwrap().hits;
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].page, Some(2));
}

#[test]
fn fuller_matches_rank_above_repeated_partial_matches() {
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(
        folder.path().join("complete.md"),
        "# Runbook\n\nRestore the saffron backup from the nightly snapshot.\n",
    )
    .unwrap();
    // More raw term frequency, but only two of the three terms.
    std::fs::write(
        folder.path().join("partial.md"),
        "# Notes\n\nbackup backup backup backup restore restore restore restore.\n",
    )
    .unwrap();
    std::fs::write(
        folder.path().join("unrelated.md"),
        "Garden watering schedule.\n",
    )
    .unwrap();
    let (store, source) = store_with_folder(folder.path());
    index_all(&store, &source, folder.path());
    let hits = store
        .search(&SearchRequest::new("restore saffron backup"))
        .unwrap()
        .hits;
    let order: Vec<_> = hits
        .iter()
        .map(|h| {
            h.path
                .as_deref()
                .unwrap()
                .rsplit(['/', '\\'])
                .next()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(order, ["complete.md", "partial.md"], "{hits:?}");
    // Turkish text matches without the diacritics too.
    std::fs::write(
        folder.path().join("kira.txt"),
        "Kira sözleşmesi: depozito tutarı bin sekiz yüz avro.\n",
    )
    .unwrap();
    index_all(&store, &source, folder.path());
    let turkish = store
        .search(&SearchRequest::new("kira depozito tutari"))
        .unwrap()
        .hits;
    assert_eq!(turkish.len(), 1);
    assert!(
        turkish[0]
            .citation
            .as_deref()
            .unwrap()
            .starts_with("[kira.txt, line 1](")
    );
}

#[test]
fn reindexing_only_changes_what_changed_and_drops_what_is_gone() {
    let folder = tempfile::tempdir().unwrap();
    for name in ["a.md", "b.md", "c.md"] {
        std::fs::write(
            folder.path().join(name),
            format!("# {name}\n\nquartz notes in {name}\n"),
        )
        .unwrap();
    }
    let (store, source) = store_with_folder(folder.path());
    assert_eq!(index_all(&store, &source, folder.path()).0, 3);
    // Nothing changed: nothing is rewritten, ids stay.
    let before: Vec<_> = store.list_documents(source.id).unwrap();
    let (changed, unchanged, _) = index_all(&store, &source, folder.path());
    assert_eq!((changed, unchanged), (0, 3));
    assert_eq!(store.list_documents(source.id).unwrap(), before);
    // One file edited, one added: exactly those are re-read.
    std::fs::write(
        folder.path().join("b.md"),
        "# b.md\n\nobsidian replaced the quartz\n",
    )
    .unwrap();
    std::fs::write(folder.path().join("d.md"), "# d.md\n\nnew quartz file\n").unwrap();
    let (changed, unchanged, _) = index_all(&store, &source, folder.path());
    assert_eq!((changed, unchanged), (2, 2));
    assert_eq!(
        store
            .search(&SearchRequest::new("obsidian replaced"))
            .unwrap()
            .hits
            .len(),
        1
    );
    // A deleted file is pruned by the caller once the scan completes; its text goes with it.
    let doc = store
        .list_documents(source.id)
        .unwrap()
        .into_iter()
        .find(|d| d.locator.starts_with("c.md"))
        .unwrap();
    let revision = store.source(source.id).unwrap().unwrap().revision;
    store.delete_document(source.id, doc.id, revision).unwrap();
    let remaining: Vec<_> = store
        .search(&SearchRequest::new("quartz notes"))
        .unwrap()
        .hits
        .into_iter()
        .map(|h| h.document_id)
        .collect();
    assert!(!remaining.contains(&doc.id));
}
