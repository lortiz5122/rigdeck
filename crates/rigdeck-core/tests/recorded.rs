//! Replays recorded link traffic through the IC-7300 driver in awkward chunk sizes.
//!
//! `fixtures/ic7300-sim-scope-425.hex` was recorded with
//! `rigdeck-tools record --rig ic7300 --scope` from the simulator (425-pixel sweeps, the length
//! seen over USB from a real IC-7300).

use rigdeck_core::protocol::Incoming;
use rigdeck_core::rig::{Event, RigKind};

fn load(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(path).unwrap().split_whitespace().map(|h| u8::from_str_radix(h, 16).unwrap()).collect()
}

fn replay(bytes: &[u8], chunk: impl Fn(usize) -> usize) -> (Vec<rigdeck_core::scope::Sweep>, usize, rigdeck_core::scope::ScopeStats) {
    let mut p = rigdeck_core::driver(RigKind::IcomIc7300);
    let mut sweeps = Vec::new();
    let mut other = 0;
    let mut i = 0;
    let mut k = 0;
    while i < bytes.len() {
        let n = chunk(k).max(1).min(bytes.len() - i);
        for m in p.feed(&bytes[i..i + n]) {
            match m {
                Incoming::Message { events, .. } => {
                    for e in events {
                        match e {
                            Event::Sweep(s) => sweeps.push(s),
                            _ => other += 1,
                        }
                    }
                }
                _ => other += 1,
            }
        }
        i += n;
        k += 1;
    }
    (sweeps, other, p.scope_stats())
}

#[test]
fn recorded_scope_stream_reassembles_identically_for_any_chunking() {
    let bytes = load("ic7300-sim-scope-425.hex");
    let (whole, other_whole, stats) = replay(&bytes, |_| bytes.len());
    assert!(whole.len() >= 10, "only {} sweeps in the recording", whole.len());
    assert!(whole.iter().all(|s| s.pixels.len() == 425));
    assert!(whole.iter().all(|s| s.pixels.iter().all(|&p| p <= 160)));
    assert_eq!(stats.dropped, 0);
    assert!(other_whole > 0, "the recording also has polls and acks");
    for chunk in [1usize, 2, 3, 7, 13, 64, 333] {
        let (s, o, st) = replay(&bytes, |_| chunk);
        assert_eq!(s, whole, "chunk {chunk}");
        assert_eq!(o, other_whole);
        assert_eq!(st.dropped, 0);
    }
    // Irregular chunks, like a USB serial driver delivers.
    let (s, _, _) = replay(&bytes, |k| [5, 61, 1, 17, 200, 3][k % 6]);
    assert_eq!(s, whole);
}

#[test]
fn corrupted_recording_drops_only_the_damaged_sweep() {
    let mut bytes = load("ic7300-sim-scope-425.hex");
    let (whole, _, _) = replay(&bytes, |_| 64);
    // Cut out the middle of the stream's fifth scope frame.
    let starts: Vec<usize> =
        bytes.windows(6).enumerate().filter(|(_, w)| *w == [0xFE, 0xFE, 0xE0, 0x94, 0x27, 0x00]).map(|(i, _)| i).collect();
    let cut = starts[20];
    bytes.drain(cut + 8..cut + 30);
    let (s, _, st) = replay(&bytes, |_| 64);
    assert_eq!(s.len(), whole.len() - 1);
    assert_eq!(st.dropped, 1);
}
