//! The side bar's terminal input as events: keys and SGR mouse reports, decoded from the bytes `watch` reads.

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
    // herdr's next and previous agent: `n`, `N`.
    Next,
    Prev,
    // herdr's peek: `p`.
    Peek,
    // views-6f: `y` answers `close? y/n`.
    Yes,
    // Any other key, an unrecognised CSI or SS3 sequence included (v6f call 12): it answers the question too.
    Other,
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

/// The sequence at the head of `bytes`: how many bytes it takes and its event, `None` for a dropped one. Any byte or
/// whole CSI/SS3 sequence that is no other key is `Other` (v6f call 12).
/// `None` overall when the sequence is unfinished.
fn decode(bytes: &[u8]) -> Option<(usize, Option<Event>)> {
    let key = match bytes[0] {
        0x03 => Event::Interrupt,
        b'\r' | b'\n' => Event::Enter,
        b'k' => Event::Up,
        b'j' => Event::Down,
        b'n' => Event::Next,
        b'N' => Event::Prev,
        b'p' => Event::Peek,
        b'y' => Event::Yes,
        ESC => return escape(bytes),
        _ => Event::Other,
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
                _ => Some(Event::Other),
            };
            Some((3, event))
        }
        _ => Some((1, Some(Event::Esc))),
    }
}

/// `\e[` then parameter and intermediate bytes (0x20-0x3F) up to a final byte (0x40-0x7E): its event, else `Other`.
/// Any other byte ends the sequence unfinished and dropped, and is decoded on its own. A sequence that reaches
/// `BUFFER_MAX` bytes without its final byte is dropped there, so it is never an event however it was read.
fn csi(bytes: &[u8]) -> Option<(usize, Option<Event>)> {
    for (i, &byte) in bytes.iter().enumerate().skip(2) {
        if i == BUFFER_MAX {
            return Some((i, None));
        }
        match byte {
            0x20..=0x3f => {}
            0x40..=0x7e => {
                let event = csi_event(&bytes[2..i], byte).unwrap_or(Event::Other);
                return Some((i + 1, Some(event)));
            }
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
mod tests;
