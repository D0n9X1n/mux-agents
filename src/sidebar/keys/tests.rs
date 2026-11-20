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
        (b"m", Event::Mail),
        (b"q", Event::Close),
        (b"i", Event::Notes),
        (b"P", Event::Placement),
        (b"o", Event::Output),
        (b" ", Event::Ack),
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
            [Read {
                event,
                first,
                alone: true
            }],
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
                    first: at(1),
                    alone: true
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
                first: at(1),
                alone: false
            },
            Read {
                event: Event::Down,
                first: at(2),
                alone: false
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
            first: at(2),
            alone: true
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
            first: at(1),
            alone: true
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
            first: at(3),
            alone: true
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
fn tab_g_capital_g_and_question_mark_are_the_hub_keys() {
    let mut decoder = Decoder::default();
    assert_eq!(
        events(&decoder.push(b"\tgG?", at(1))),
        [Event::Tab, Event::First, Event::Last, Event::Help]
    );
}

#[test]
fn q_i_capital_p_o_and_space_are_the_hub_keys_too() {
    let mut decoder = Decoder::default();
    assert_eq!(
        events(&decoder.push(b"qiPo ", at(1))),
        [
            Event::Close,
            Event::Notes,
            Event::Placement,
            Event::Output,
            Event::Ack
        ]
    );
}

// `close bar? y/n` takes a `y` only when it came alone: a paste or a key held down reads more than one event at once.
#[test]
fn an_event_is_alone_only_when_its_read_brought_no_other() {
    let mut decoder = Decoder::default();
    let alone = |reads: Vec<Read>| reads.iter().map(|r| r.alone).collect::<Vec<_>>();
    assert_eq!(alone(decoder.push(b"y", at(1))), [true]);
    assert_eq!(alone(decoder.push(b"qy", at(2))), [false, false]);
    assert_eq!(alone(decoder.push(b"\x1by", at(3))), [false, false]);
    // A sequence finished by a later read is alone in that read.
    assert_eq!(decoder.push(b"\x1b[", at(4)), []);
    assert_eq!(alone(decoder.push(b"A", at(5))), [true]);
}

// A `y` is alone only when its read was that one key: an unfinished sequence left buffered after it, or a dropped one
// in the same read, is more than the key, so a paste that happens to decode to one `y` never answers `close bar?`.
#[test]
fn a_key_is_alone_only_when_its_read_was_that_key_and_nothing_else() {
    let alone = |bytes: &[u8]| {
        let reads = Decoder::default().push(bytes, at(1));
        reads.iter().map(|r| r.alone).collect::<Vec<_>>()
    };
    assert_eq!(alone(b"y"), [true]);
    assert_eq!(alone(b"yy"), [false, false]);
    // `\e[` left unfinished after the `y`.
    assert_eq!(alone(b"y\x1b["), [false]);
    // A lone ESC after the `y`, which may still become a sequence.
    assert_eq!(alone(b"y\x1b"), [false]);
    // A sequence dropped at `BUFFER_MAX`, then the `y`: the dropped bytes were read with it.
    let mut cut = b"\x1b[".to_vec();
    cut.extend([b'0'; BUFFER_MAX - 2]);
    cut.push(b'y');
    assert_eq!(alone(&cut), [false]);
}

// `@mux-agents-keys`: each `action=key` moves that action to one printable ASCII key, which no other action keeps;
// an unknown action, a key that is not one printable character, and `y` (the answer to a question) are ignored.
#[test]
fn the_keymap_moves_each_named_action_to_its_key() {
    let map = Keymap::parse("notes=I, close = Q,stop=x,peek=ab,,help=y,mail=,=z");
    assert_eq!(map.ignored(), ["stop", "peek", "help", "mail", "=z"]);
    let mut decoder = Decoder::new(map.clone());
    assert_eq!(
        events(&decoder.push(b"IQiqpm?y", at(1))),
        [
            Event::Notes,
            Event::Close,
            Event::Other,
            Event::Other,
            Event::Peek,
            Event::Mail,
            Event::Help,
            Event::Yes
        ]
    );
    assert_eq!(
        (map.key(Event::Notes), map.key(Event::Peek)),
        (Some(b'I'), Some(b'p'))
    );
    // Taking another action's key leaves that action without one; a swap gives each the other's.
    let map = Keymap::parse("notes=j,peek=n,next=p");
    assert_eq!(map.key(Event::Down), None);
    assert_eq!(
        events(&Decoder::new(map.clone()).push(b"jnpN", at(1))),
        [Event::Notes, Event::Peek, Event::Next, Event::Prev]
    );
    // Arrows still move; the fixed keys stay fixed.
    assert_eq!(
        events(&Decoder::new(map).push(b"\x1b[B\r\t", at(1))),
        [Event::Down, Event::Enter, Event::Tab]
    );
    // Space is a key too, shown as `Space`; the default map is the keys as shipped.
    let map = Keymap::parse("ack=., close= ");
    assert_eq!(
        (map.label(Event::Ack), map.label(Event::Close)),
        (Some(".".to_string()), Some("Space".to_string()))
    );
    assert_eq!(Keymap::default(), Keymap::parse(""));
    assert_eq!(
        Keymap::default().label(Event::Ack).as_deref(),
        Some("Space")
    );
}

#[test]
fn garbage_is_other_and_never_panics() {
    let keys = [
        0x03, b'\r', b'\n', b'j', b'k', b'n', b'N', b'p', b'm', b'y', b'\t', b'g', b'G', b'?',
        b'q', b'i', b'P', b'o', b' ', 0x1b,
    ];
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
            first: at(2),
            alone: true
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
