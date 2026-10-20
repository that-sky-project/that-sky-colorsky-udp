use super::{SnapshotReader, SnapshotWriter, snapshot_checksum};

#[test]
fn roundtrip() {
    let mut writer = SnapshotWriter::new();
    let mut reader = SnapshotReader::new();

    // First frame is a keyframe against the seeded empty seq-1 base.
    let state1 = vec![0xAAu8; 100];
    let frame1 = writer.generate_delta(&state1);
    assert_eq!(frame1[0], 2);
    assert_eq!(frame1[1], 1);
    assert_eq!(reader.apply_delta(&frame1).unwrap(), state1);

    // Before an ack, deltas still base on the oldest unacked frame (seq 1).
    let state2 = vec![0xBBu8; 100];
    let frame2 = writer.generate_delta(&state2);
    assert_eq!(frame2[0], 0);
    assert_eq!(frame2[1], 1);
    assert_eq!(reader.apply_delta(&frame2).unwrap(), state2);

    // Acking the keyframe prunes the seed, so the next delta bases on seq 2.
    assert!(writer.ack(2));
    let state3 = vec![0xCCu8; 100];
    let frame3 = writer.generate_delta(&state3);
    assert_eq!(frame3[0], 0);
    assert_eq!(frame3[1], 2);
    assert_eq!(reader.apply_delta(&frame3).unwrap(), state3);

    // An unknown sequence is not acknowledged.
    assert!(!writer.ack(0x7f));
}

#[test]
fn keyframe_interval() {
    let mut writer = SnapshotWriter::new();
    let mut reader = SnapshotReader::new();

    for i in 0..12u8 {
        let state = vec![i; 64];
        let frame = writer.generate_delta(&state);
        if i == 0 {
            assert_eq!(frame[0], 2);
            assert_eq!(frame[1], 1);
        } else if i == 10 {
            assert_eq!(frame[0], 3);
            assert_eq!(frame[1], 1);
        } else {
            assert_eq!(frame[0], 0);
        }
        assert_eq!(reader.apply_delta(&frame).unwrap(), state);
    }
}

#[test]
fn force_keyframe_resets_to_seed() {
    let mut writer = SnapshotWriter::new();
    let _ = writer.generate_delta(&[0x11u8; 32]);
    writer.force_keyframe();

    let frame = writer.generate_delta(&[0x22u8; 32]);
    assert_eq!(frame[0], 2);
    assert_eq!(frame[1], 1);
}

#[test]
fn missing_base_requires_raw_frame() {
    let mut reader = SnapshotReader::new();
    let mut writer = SnapshotWriter::new();

    let state1 = vec![0x11u8; 32];
    let keyframe = writer.generate_delta(&state1);
    let delta = writer.generate_delta(&[0x22u8; 32]);

    // A delta referencing an unknown base resets the reader.
    let mut bad = vec![0u8, 99];
    let payload = vec![0x33u8; 8];
    bad.push(snapshot_checksum(&payload));
    bad.extend_from_slice(&payload);
    assert!(reader.apply_delta(&bad).is_none());
    assert!(reader.apply_delta(&delta).is_none());

    // Only a raw frame restores the reader.
    let raw_state = vec![0x44u8; 32];
    let mut raw = vec![7u8, 0];
    raw.push(snapshot_checksum(&raw_state));
    raw.extend_from_slice(&raw_state);
    assert_eq!(reader.apply_delta(&raw).unwrap(), raw_state);

    // The original keyframe is no longer usable after the reset.
    assert!(reader.apply_delta(&keyframe).is_none());
}

#[test]
fn window_overflow_enters_resync() {
    let mut writer = SnapshotWriter::new();
    let mut saw_raw = false;

    for i in 0..200u8 {
        let frame = writer.generate_delta(&[i; 16]);
        if i > 0 && frame[0] != 0 && frame[1] == 0 {
            saw_raw = true;
            break;
        }
    }

    assert!(
        saw_raw,
        "writer should emit a raw frame after the window fills"
    );
}
