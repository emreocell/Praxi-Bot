use live_view::wire::{Decoder, Record};
use live_view::*;
use std::sync::Arc;

fn fixture() -> FrameMetadata {
    serde_json::from_value(
        serde_json::from_str::<serde_json::Value>(include_str!("fixtures/frame.json")).unwrap(),
    )
    .unwrap()
}
#[test]
fn golden_binary_and_json_headers_round_trip() {
    for (bytes, json) in [
        (
            include_bytes!("fixtures/heartbeat.bin").as_slice(),
            include_str!("fixtures/heartbeat.json"),
        ),
        (
            include_bytes!("fixtures/frame.bin").as_slice(),
            include_str!("fixtures/frame.json"),
        ),
    ] {
        let mut decoder = Decoder::default();
        let (used, record) = decoder.feed(bytes).unwrap();
        assert_eq!(used, bytes.len());
        let record = record.unwrap();
        assert_eq!(
            serde_json::to_value(&record.header).unwrap(),
            serde_json::from_str::<serde_json::Value>(json).unwrap()
        );
        assert_eq!(record.encode().unwrap(), bytes);
        decoder.finish().unwrap();
    }
}
#[test]
fn every_fragment_boundary_and_concatenated_records() {
    let bytes = include_bytes!("fixtures/frame.bin");
    for split in 0..bytes.len() {
        let mut d = Decoder::default();
        let (_, one) = d.feed(&bytes[..split]).unwrap();
        assert!(one.is_none());
        let (_, two) = d.feed(&bytes[split..]).unwrap();
        assert!(two.is_some());
        d.finish().unwrap();
    }
    let mut d = Decoder::default();
    for (i, b) in bytes.iter().enumerate() {
        let (_, record) = d.feed(&[*b]).unwrap();
        assert_eq!(record.is_some(), i == bytes.len() - 1);
    }
    let mut concatenated = bytes.to_vec();
    concatenated.extend_from_slice(bytes);
    let mut d = Decoder::default();
    let (n, one) = d.feed(&concatenated).unwrap();
    assert!(one.is_some());
    assert!(d.feed(&concatenated[n..]).unwrap().1.is_some());
}
#[test]
fn oversized_lengths_reject_before_body_allocation_and_poison_parser() {
    for (h, p) in [
        (8193u32, 0u32),
        (1, 2 * 1024 * 1024 + 1),
        (u32::MAX, u32::MAX),
        (0, 0),
    ] {
        let mut prefix = h.to_be_bytes().to_vec();
        prefix.extend_from_slice(&p.to_be_bytes());
        let mut d = Decoder::default();
        assert_eq!(d.feed(&prefix).err(), Some(ViewError::LimitExceeded));
        assert!(d.feed(&[]).is_err());
    }
}
#[test]
fn truncation_bad_json_version_and_nonframe_payload_fail() {
    let mut d = Decoder::default();
    d.feed(&[0, 0, 0]).unwrap();
    assert!(d.finish().is_err());
    assert!(
        Record {
            header: RecordHeader::Heartbeat {
                v: 2,
                server_time_ms: 0
            },
            payload: vec![]
        }
        .encode()
        .is_err()
    );
    assert!(
        Record {
            header: RecordHeader::Heartbeat {
                v: 1,
                server_time_ms: 0
            },
            payload: vec![1]
        }
        .encode()
        .is_err()
    );
    let mut d = Decoder::default();
    assert!(d.feed(&[0, 0, 0, 1, 0, 0, 0, 0, b'x']).is_err());
}
#[tokio::test]
async fn ten_thousand_frames_keep_only_latest_and_drop_pixels_after_last_subscriber() {
    let source = SyntheticSource::default();
    let mut sub = source.subscribe(EffectiveProfile::watch()).await.unwrap();
    let bytes: Arc<[u8]> = Arc::from([255, 216, 255, 217]);
    let weak = Arc::downgrade(&bytes);
    for seq in 0..10_000 {
        let mut metadata = fixture();
        metadata.frame_seq = seq;
        source
            .publish(Frame {
                metadata,
                payload: Arc::clone(&bytes),
            })
            .unwrap();
    }
    assert_eq!(source.subscribers(), 1);
    assert_eq!(
        sub.frames
            .borrow_and_update()
            .as_ref()
            .unwrap()
            .metadata
            .frame_seq,
        9999
    );
    assert_eq!(Arc::strong_count(&bytes), 2, "one source frame, no backlog");
    drop(bytes);
    drop(sub);
    assert_eq!(source.subscribers(), 0);
    assert!(
        weak.upgrade().is_none(),
        "pixels retained after last unsubscribe"
    );
}
#[tokio::test]
async fn source_rejects_native_pixel_bombs_and_empty_payloads() {
    let source = SyntheticSource::default();
    let mut metadata = fixture();
    metadata.source_rect.width = 100_000;
    assert!(
        source
            .publish(Frame {
                metadata,
                payload: Arc::from([1])
            })
            .is_err()
    );
    assert!(
        source
            .publish(Frame {
                metadata: fixture(),
                payload: Arc::from([])
            })
            .is_err()
    );
}
#[test]
fn typed_input_bounds_and_no_raw_os_keycodes() {
    for (event, valid) in [
        (
            serde_json::json!({"type":"text","text":"x".repeat(4096)}),
            true,
        ),
        (
            serde_json::json!({"type":"text","text":"x".repeat(4097)}),
            false,
        ),
        (
            serde_json::json!({"type":"wheel","x":0.5,"y":0.5,"delta_x":1201,"delta_y":0,"modifiers":[]}),
            false,
        ),
        (
            serde_json::json!({"type":"key","phase":"down","code":"KeyA","key":"a","location":0,"repeat":false,"modifiers":[]}),
            true,
        ),
        (
            serde_json::json!({"type":"key","phase":"down","code":"0x41","key":"a","location":0,"repeat":false,"modifiers":[]}),
            false,
        ),
    ] {
        let e: InputEvent = serde_json::from_value(event).unwrap();
        assert_eq!(e.valid(), valid);
    }
    assert!(
        serde_json::from_value::<InputEvent>(
            serde_json::json!({"type":"text","text":"x","actor":"other"})
        )
        .is_err()
    );
}
