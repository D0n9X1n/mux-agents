use super::*;
use std::time::Duration;

fn at(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

fn events(reads: &[Read]) -> Vec<Event> {
    reads.iter().map(|read| read.event).collect()
}

fn table() -> Vec<(&'static [u8], Event)> {
    vec![
        (b"\x03", Event::Interrupt),
        (b"\r", Event::Enter),
        (b"\n", Event::Enter),
        (b"k", Event::Up),
        (b"\x1b[A", Event::Up),
        (b"\x1bOA", Event::Up),
        (b"j", Event::Down),
        (b"n", Event::Next),
        (b"N", Event::Prev),
        (b"p", Event::Peek),
        (b"\x1b[B", Event::Down),
        (b"\x1bOB", Event::Down),
        (b"y", Event::Yes),
        (b"x", Event::Other),
        (b"\x1b[C", Event::Other),
        (b"\x1b[5~", Event::Other),
        (b"\x1bOP", Event::Other),
        (
            b"\x1b[<0;3;5M",
            Event::Press {
                button: 0,
                x: 3,
                y: 5,
            },
        ),
        (
            b"\x1b[<64;120;48M",
            Event::Press {
                button: 64,
                x: 120,
                y: 48,
            },
        ),
        (
            b"\x1b[<2;65535;1m",
            Event::Release {
                button: 2,
                x: 65535,
                y: 1,
            },
        ),
    ]
}

#[test]
fn decodes_every_event_whole() {
    for (bytes, event) in table() {
        let mut decoder = Decoder::default();
        let first = at(7);
        assert_eq!(
            decoder.push(bytes, first),
            [Read { event, first }],
            "{bytes:?}"
        );
        assert!(decoder.buf.is_empty(), "{bytes:?}");
    }
}

#[test]
fn decodes_every_event_split_at_each_byte() {
    for (bytes, event) in table() {
        for split in 1..=bytes.len() {
            let (a, b) = bytes.split_at(split);
            let mut decoder = Decoder::default();
            let mut reads = decoder.push(a, at(1));
            reads.extend(decoder.push(b, at(2)));
            assert_eq!(
                reads,
                [Read {
                    event,
                    first: at(1)
                }],
                "{a:?} {b:?}"
            );
        }
    }
}

#[test]
fn first_byte_time_of_each_event_in_one_read() {
    let mut decoder = Decoder::default();
    assert_eq!(decoder.push(b"\x1b[<0;3;", at(1)), []);
    assert_eq!(
        decoder.push(b"5Mj", at(2)),
        [
            Read {
                event: Event::Press {
                    button: 0,
                    x: 3,
                    y: 5
                },
                first: at(1)
            },
            Read {
                event: Event::Down,
                first: at(2)
            },
        ]
    );
}

#[test]
fn a_new_unfinished_sequence_takes_its_own_read_time() {
    // A report finishes, then a new one starts in the same read: the new one's first byte was read at t2, not at
    // t1 when the earlier sequence started.
    let mut decoder = Decoder::default();
    assert_eq!(decoder.push(b"\x1b[<0;3;", at(1)), []);
    assert_eq!(events(&decoder.push(b"5M\x1b[<0;4;", at(2))).len(), 1);
    assert_eq!(
        decoder.push(b"6M", at(3)),
        [Read {
            event: Event::Press {
                button: 0,
                x: 4,
                y: 6
            },
            first: at(2)
        }]
    );
}

// The events that are not `Other`: what a dropped or unknown sequence must never turn into.
fn known(reads: &[Read]) -> Vec<Event> {
    events(reads)
        .into_iter()
        .filter(|e| *e != Event::Other)
        .collect()
}

#[test]
fn an_unknown_csi_takes_its_final_byte_with_it() {
    // `\e[5~` (Page Up) is one `Other` (v6f call 12): its final `~` is not left to be read on its own, and an
    // unknown CSI ending in `j` or `k` takes that letter too, so it is no Down or Up.
    let mut decoder = Decoder::default();
    assert_eq!(events(&decoder.push(b"\x1b[5~", at(1))), [Event::Other]);
    assert_eq!(
        events(&decoder.push(b"\x1b[2j\x1b[3k", at(1))),
        [Event::Other, Event::Other]
    );
    assert!(decoder.buf.is_empty());
}

#[test]
fn idle_turns_a_lone_escape_into_esc() {
    let mut decoder = Decoder::default();
    assert_eq!(decoder.push(b"\x1b", at(1)), []);
    assert!(decoder.lone_esc());
    assert_eq!(
        decoder.idle(),
        [Read {
            event: Event::Esc,
            first: at(1)
        }]
    );
    assert!(!decoder.lone_esc());
    assert_eq!(decoder.idle(), []);

    assert_eq!(decoder.push(b"\x1b[", at(3)), []);
    assert!(!decoder.lone_esc());
    assert_eq!(decoder.idle(), []);
    assert_eq!(
        decoder.push(b"A", at(4)),
        [Read {
            event: Event::Up,
            first: at(3)
        }]
    );
}

#[test]
fn escape_then_another_byte() {
    let mut decoder = Decoder::default();
    assert_eq!(
        events(&decoder.push(b"\x1bj", at(1))),
        [Event::Esc, Event::Down]
    );
    assert_eq!(events(&decoder.push(b"\x1b\x1b", at(2))), [Event::Esc]);
    assert!(decoder.lone_esc());
    let mut decoder = Decoder::default();
    assert_eq!(
        events(&decoder.push(b"\x1bx", at(3))),
        [Event::Esc, Event::Other]
    );
}

#[test]
fn garbage_is_other_and_never_panics() {
    let keys = [0x03, b'\r', b'\n', b'j', b'k', b'n', b'N', b'p', b'y', 0x1b];
    for byte in 0..=255u8 {
        let mut decoder = Decoder::default();
        let reads = decoder.push(&[byte], at(1));
        if keys.contains(&byte) {
            assert!(reads.len() <= 1, "{byte}");
        } else {
            assert_eq!(events(&reads), [Event::Other], "{byte}");
        }
    }

    let mut decoder = Decoder::default();
    assert_eq!(
        events(&decoder.push(b"\x1b[5~j", at(1))),
        [Event::Other, Event::Down]
    );
    assert_eq!(
        events(&decoder.push(b"\x1bOPj", at(1))),
        [Event::Other, Event::Down]
    );
    assert_eq!(
        known(&decoder.push(b"\x1b[<0;3M\x1b[<a;1;1Mj", at(1))),
        [Event::Down]
    );

    let mut seed: u64 = 0x5eed;
    let mut next = || {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) as usize
    };
    let alphabet = b"\x1b[O<;0123456789Mmjk\r\n\x03ABx~";
    let mut decoder = Decoder::default();
    for round in 0..10_000 {
        let len = next() % 24;
        let bytes: Vec<u8> = (0..len)
            .map(|_| match next() % 4 {
                0 => next() as u8,
                _ => alphabet[next() % alphabet.len()],
            })
            .collect();
        decoder.push(&bytes, at(round));
        assert!(decoder.buf.len() <= BUFFER_MAX, "{bytes:?}");
        if next() % 8 == 0 {
            decoder.idle();
        }
    }
}

#[test]
fn overflow_drops_the_buffer() {
    let mut decoder = Decoder::default();
    let mut long = b"\x1b[<".to_vec();
    long.extend([b'7'; 70]);
    // Dropped at the limit; the digits read after it are each `Other`, never a report.
    assert_eq!(known(&decoder.push(&long, at(1))), []);
    assert!(decoder.buf.is_empty());
    assert_eq!(
        decoder.push(b"j", at(2)),
        [Read {
            event: Event::Down,
            first: at(2)
        }]
    );
    // A field past u16 makes its whole report `Other`.
    assert_eq!(
        events(&decoder.push(b"\x1b[<0;65536;1Mj", at(3))),
        [Event::Other, Event::Down]
    );
}

#[test]
fn a_report_longer_than_the_buffer_is_dropped_however_it_is_read() {
    // 78 bytes: whole in one read, or split at any byte, it is never a report (its tail past the limit is read as
    // `Other` keys).
    let mut long = b"\x1b[<".to_vec();
    long.extend([b'0'; 70]);
    long.extend(b";3;5M");
    for split in 0..=long.len() {
        let mut decoder = Decoder::default();
        let (a, b) = long.split_at(split);
        let mut got = known(&decoder.push(a, at(1)));
        got.extend(known(&decoder.push(b, at(2))));
        got.extend(known(&decoder.push(b"j", at(3))));
        assert_eq!(got, [Event::Down], "split at {split}");
    }
    // The limit is 64 bytes. One past it is dropped too, however it is read.
    assert_eq!(BUFFER_MAX, 64);
    let mut over = b"\x1b[<".to_vec();
    over.extend([b'0'; 57]);
    over.extend(b";3;5M");
    assert_eq!(over.len(), 65);
    for split in 0..=over.len() {
        let mut decoder = Decoder::default();
        let (a, b) = over.split_at(split);
        let mut got = known(&decoder.push(a, at(1)));
        got.extend(known(&decoder.push(b, at(2))));
        assert_eq!(got, [], "split at {split}");
    }
    // One at the limit still decodes.
    let mut fits = b"\x1b[<".to_vec();
    fits.extend([b'0'; 56]);
    fits.extend(b";3;5M");
    assert_eq!(fits.len(), 64);
    assert_eq!(
        events(&Decoder::default().push(&fits, at(1))),
        [Event::Press {
            button: 0,
            x: 3,
            y: 5
        }]
    );
}

#[test]
fn rmux_press_and_release_in_separate_reads() {
    let mut decoder = Decoder::default();
    assert_eq!(
        events(&decoder.push(b"\x1b[<0;3;5M", at(1))),
        [Event::Press {
            button: 0,
            x: 3,
            y: 5
        }]
    );
    assert_eq!(
        events(&decoder.push(b"\x1b[<0;3;5m", at(2))),
        [Event::Release {
            button: 0,
            x: 3,
            y: 5
        }]
    );
}
