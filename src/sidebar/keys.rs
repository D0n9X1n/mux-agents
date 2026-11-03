//! The side bar's terminal input as events: keys and SGR mouse reports, decoded from the bytes `watch` reads.
#![allow(dead_code)] // Until T9 gives every item a caller (#61).

use std::time::SystemTime;

/// One input event the side bar acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Press { button: u16, x: u16, y: u16 },
    Release { button: u16, x: u16, y: u16 },
    Up,
    Down,
    Enter,
    Esc,
    Interrupt,
}

/// An event and the time its first byte was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Read {
    pub event: Event,
    pub first: SystemTime,
}

/// The most bytes of an unfinished sequence the decoder keeps; past it the buffer is cleared.
pub const BUFFER_MAX: usize = 64;

/// Turns bytes into events as they arrive, keeping an unfinished sequence until its last byte comes.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
    since: Option<SystemTime>,
}

impl Decoder {
    /// Bytes read at `now`; an event completing a buffered sequence keeps its first byte's read time.
    /// Bytes kept from earlier reads are decoded first; an event that starts in them takes `since`, any later one `now`.
    pub fn push(&mut self, bytes: &[u8], now: SystemTime) -> Vec<Read> {
        let carried = self.buf.len();
        let since = self.since.unwrap_or(now);
        self.buf.extend_from_slice(bytes);
        let mut reads = Vec::new();
        let mut at = 0;
        while at < self.buf.len() {
            let Some((len, event)) = decode(&self.buf[at..]) else {
                break;
            };
            if let Some(event) = event {
                let first = if at < carried { since } else { now };
                reads.push(Read { event, first });
            }
            at += len;
        }
        self.buf.drain(..at);
        if self.buf.len() > BUFFER_MAX {
            self.buf.clear();
        }
        self.since = if self.buf.is_empty() {
            None
        } else if at < carried {
            Some(since)
        } else {
            Some(now)
        };
        reads
    }

    /// A pending lone ESC becomes Esc; anything else pending stays.
    pub fn idle(&mut self) -> Vec<Read> {
        if !self.lone_esc() {
            return Vec::new();
        }
        self.buf.clear();
        let first = self.since.take().unwrap_or(SystemTime::UNIX_EPOCH);
        vec![Read {
            event: Event::Esc,
            first,
        }]
    }

    /// Whether the buffer holds only an ESC, which is Esc unless more bytes follow soon.
    pub fn lone_esc(&self) -> bool {
        self.buf == [ESC]
    }
}

const ESC: u8 = 0x1b;

/// The sequence at the head of `bytes`: how many bytes it takes and its event, `None` for a dropped one.
/// `None` overall when the sequence is unfinished.
fn decode(bytes: &[u8]) -> Option<(usize, Option<Event>)> {
    let key = match bytes[0] {
        0x03 => Event::Interrupt,
        b'\r' | b'\n' => Event::Enter,
        b'k' => Event::Up,
        b'j' => Event::Down,
        ESC => return escape(bytes),
        _ => return Some((1, None)),
    };
    Some((1, Some(key)))
}

/// An ESC and what follows it: a CSI, an SS3, or a bare Esc that leaves the next byte to be decoded alone.
fn escape(bytes: &[u8]) -> Option<(usize, Option<Event>)> {
    match bytes.get(1)? {
        b'[' => csi(bytes),
        b'O' => {
            let event = match bytes.get(2)? {
                b'A' => Some(Event::Up),
                b'B' => Some(Event::Down),
                _ => None,
            };
            Some((3, event))
        }
        _ => Some((1, Some(Event::Esc))),
    }
}

/// `\e[` then parameter and intermediate bytes (0x20-0x3F) up to a final byte (0x40-0x7E).
/// Any other byte ends the sequence unfinished and dropped, and is decoded on its own. A sequence that reaches
/// `BUFFER_MAX` bytes without its final byte is dropped there, so it is never an event however it was read.
fn csi(bytes: &[u8]) -> Option<(usize, Option<Event>)> {
    for (i, &byte) in bytes.iter().enumerate().skip(2) {
        if i == BUFFER_MAX {
            return Some((i, None));
        }
        match byte {
            0x20..=0x3f => {}
            0x40..=0x7e => return Some((i + 1, csi_event(&bytes[2..i], byte))),
            _ => return Some((i, None)),
        }
    }
    None
}

/// The event a whole CSI stands for: an arrow key, an SGR mouse report `<b;x;y` with `M` or `m`, or none.
fn csi_event(params: &[u8], last: u8) -> Option<Event> {
    match (params, last) {
        (b"", b'A') => return Some(Event::Up),
        (b"", b'B') => return Some(Event::Down),
        _ => {}
    }
    let report = params.strip_prefix(b"<")?;
    let mut fields = report.split(|&byte| byte == b';').map(number);
    let (button, x, y) = (fields.next()??, fields.next()??, fields.next()??);
    if fields.next().is_some() {
        return None;
    }
    match last {
        b'M' => Some(Event::Press { button, x, y }),
        b'm' => Some(Event::Release { button, x, y }),
        _ => None,
    }
}

/// Decimal digits as a `u16`; `None` when empty, not digits, or past `u16::MAX`.
fn number(digits: &[u8]) -> Option<u16> {
    if digits.is_empty() {
        return None;
    }
    digits.iter().try_fold(0u16, |n, &digit| {
        if !digit.is_ascii_digit() {
            return None;
        }
        n.checked_mul(10)?.checked_add(u16::from(digit - b'0'))
    })
}

#[cfg(test)]
mod tests {
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
            (b"\x1b[B", Event::Down),
            (b"\x1bOB", Event::Down),
            (b"\x1bx", Event::Esc),
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

    #[test]
    fn an_unknown_csi_takes_its_final_byte_with_it() {
        // `\e[5~` (Page Up) is dropped whole: its final `~` is not left to be read on its own, and an unknown CSI
        // ending in `j` or `k` takes that letter too, so it is no Down or Up.
        let mut decoder = Decoder::default();
        assert_eq!(events(&decoder.push(b"\x1b[5~", at(1))), []);
        assert_eq!(events(&decoder.push(b"\x1b[2j\x1b[3k", at(1))), []);
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
    }

    #[test]
    fn garbage_is_ignored_and_never_panics() {
        let keys = [0x03, b'\r', b'\n', b'j', b'k', 0x1b];
        for byte in 0..=255u8 {
            let mut decoder = Decoder::default();
            let reads = decoder.push(&[byte], at(1));
            if keys.contains(&byte) {
                assert!(reads.len() <= 1, "{byte}");
            } else {
                assert_eq!(reads, [], "{byte}");
            }
        }

        let mut decoder = Decoder::default();
        assert_eq!(events(&decoder.push(b"\x1b[5~j", at(1))), [Event::Down]);
        assert_eq!(events(&decoder.push(b"\x1bOPj", at(1))), [Event::Down]);
        assert_eq!(
            events(&decoder.push(b"\x1b[<0;3M\x1b[<a;1;1Mj", at(1))),
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
        assert_eq!(decoder.push(&long, at(1)), []);
        assert!(decoder.buf.is_empty());
        assert_eq!(
            decoder.push(b"j", at(2)),
            [Read {
                event: Event::Down,
                first: at(2)
            }]
        );
        // A field past u16 drops its whole report.
        assert_eq!(
            events(&decoder.push(b"\x1b[<0;65536;1Mj", at(3))),
            [Event::Down]
        );
    }

    #[test]
    fn a_report_longer_than_the_buffer_is_dropped_however_it_is_read() {
        // 78 bytes: whole in one read, or split at any byte, it is never an event.
        let mut long = b"\x1b[<".to_vec();
        long.extend([b'0'; 70]);
        long.extend(b";3;5M");
        for split in 0..=long.len() {
            let mut decoder = Decoder::default();
            let (a, b) = long.split_at(split);
            let mut got = events(&decoder.push(a, at(1)));
            got.extend(events(&decoder.push(b, at(2))));
            got.extend(events(&decoder.push(b"j", at(3))));
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
            let mut got = events(&decoder.push(a, at(1)));
            got.extend(events(&decoder.push(b, at(2))));
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
}
