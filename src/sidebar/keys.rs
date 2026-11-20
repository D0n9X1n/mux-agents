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
    // The hub: `m` shows the selected row's unread mail, read-only.
    Mail,
    // Tab opens or closes the ended fold; `g` and `G` select the first and last row; `?` shows the keys.
    Tab,
    First,
    Last,
    Help,
    // `q` asks `close bar? y/n`; `i` shows the team's notes; `P` cycles where new agents open; `o` shows the
    // selected row's retained output; Space marks the selected done agent seen.
    Close,
    Notes,
    Placement,
    Output,
    Ack,
    // Any other key, an unrecognised CSI or SS3 sequence included (v6f call 12): it answers the question too.
    Other,
}

/// An event, the time its first byte was read, and whether its read was that one key and nothing else: a paste or a
/// key held down brings several at once, and bytes dropped or left waiting beside it make it no lone key either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Read {
    pub event: Event,
    pub first: SystemTime,
    pub alone: bool,
}

/// The keys an action can be moved to with `@mux-agents-keys` (`action=key`, comma-separated), and each one's key as
/// shipped. Enter, Tab, Esc, the arrows and Ctrl-C stay where they are, and `y` stays the answer to a question.
const ACTIONS: [(&str, Event, u8); 14] = [
    ("down", Event::Down, b'j'),
    ("up", Event::Up, b'k'),
    ("first", Event::First, b'g'),
    ("last", Event::Last, b'G'),
    ("next", Event::Next, b'n'),
    ("prev", Event::Prev, b'N'),
    ("peek", Event::Peek, b'p'),
    ("output", Event::Output, b'o'),
    ("mail", Event::Mail, b'm'),
    ("notes", Event::Notes, b'i'),
    ("placement", Event::Placement, b'P'),
    ("ack", Event::Ack, b' '),
    ("close", Event::Close, b'q'),
    ("help", Event::Help, b'?'),
];

/// The printable keys (Space to `~`) and the action each one is, as `@mux-agents-keys` left them, with the entries it
/// ignored. Read once when the bar starts; decoding looks a key up here, so `act` only ever sees events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    keys: [Event; 95],
    ignored: Vec<String>,
}

impl Default for Keymap {
    fn default() -> Self {
        let mut keys = [Event::Other; 95];
        for (_, event, key) in ACTIONS {
            keys[usize::from(key - b' ')] = event;
        }
        Keymap {
            keys,
            ignored: Vec::new(),
        }
    }
}

impl Keymap {
    /// The keys as shipped, then each `action=key` of `value` in order: the action leaves its old key and takes `key`,
    /// which any other action holding it loses. An entry naming no known action, or whose key is not exactly one
    /// printable ASCII character other than `y`, is ignored and kept (by its name) for `?` to list.
    pub fn parse(value: &str) -> Keymap {
        let mut map = Keymap::default();
        for entry in value.split(',') {
            if entry.trim().is_empty() {
                continue;
            }
            let (name, key) = entry.split_once('=').unwrap_or((entry, ""));
            let name = name.trim();
            // Spaces around a key are dropped, unless the key is a space.
            let key = match key.trim() {
                "" => key,
                trimmed => trimmed,
            };
            let action = ACTIONS.iter().find(|(known, ..)| *known == name);
            let key = match key.as_bytes() {
                [byte @ b' '..=b'~'] if *byte != b'y' => Some(*byte),
                _ => None,
            };
            match (action, key) {
                (Some((_, event, _)), Some(key)) => {
                    for slot in map.keys.iter_mut().filter(|slot| *slot == event) {
                        *slot = Event::Other;
                    }
                    map.keys[usize::from(key - b' ')] = *event;
                }
                _ => map.ignored.push(if name.is_empty() {
                    entry.trim().to_string()
                } else {
                    name.to_string()
                }),
            }
        }
        map
    }

    /// The event printable `byte` is, `Other` for any other byte.
    fn event(&self, byte: u8) -> Event {
        match byte {
            b' '..=b'~' => self.keys[usize::from(byte - b' ')],
            _ => Event::Other,
        }
    }

    /// `event`'s key, none when it has none (another action took it).
    pub fn key(&self, event: Event) -> Option<u8> {
        (b' '..=b'~').find(|&byte| self.keys[usize::from(byte - b' ')] == event)
    }

    /// `event`'s key as the bar writes it: `Space` for the space, else the character.
    pub fn label(&self, event: Event) -> Option<String> {
        self.key(event).map(|key| match key {
            b' ' => "Space".to_string(),
            key => char::from(key).to_string(),
        })
    }

    /// The entries `parse` ignored, by name.
    pub fn ignored(&self) -> &[String] {
        &self.ignored
    }
}

/// The most bytes of an unfinished sequence the decoder keeps; past it the buffer is cleared.
pub const BUFFER_MAX: usize = 64;

/// Turns bytes into events as they arrive, keeping an unfinished sequence until its last byte comes.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
    since: Option<SystemTime>,
    keys: Keymap,
}

impl Decoder {
    /// A decoder whose printable keys are `keys`.
    pub fn new(keys: Keymap) -> Decoder {
        Decoder {
            keys,
            ..Decoder::default()
        }
    }

    /// Bytes read at `now`; an event completing a buffered sequence keeps its first byte's read time.
    /// Bytes kept from earlier reads are decoded first; an event that starts in them takes `since`, any later one `now`.
    pub fn push(&mut self, bytes: &[u8], now: SystemTime) -> Vec<Read> {
        let carried = self.buf.len();
        let since = self.since.unwrap_or(now);
        self.buf.extend_from_slice(bytes);
        let mut reads = Vec::new();
        let mut at = 0;
        // Bytes this read brought that no event took: a dropped sequence, or one cleared at `BUFFER_MAX`.
        let mut dropped = false;
        while at < self.buf.len() {
            let Some((len, event)) = decode(&self.buf[at..], &self.keys) else {
                break;
            };
            match event {
                Some(event) => {
                    let first = if at < carried { since } else { now };
                    reads.push(Read {
                        event,
                        first,
                        alone: false,
                    });
                }
                None => dropped = true,
            }
            at += len;
        }
        self.buf.drain(..at);
        if self.buf.len() > BUFFER_MAX {
            self.buf.clear();
            dropped = true;
        }
        // Alone: the read was that one key and nothing else, none dropped and none left waiting for more bytes.
        if let [read] = reads.as_mut_slice()
            && !dropped
            && self.buf.is_empty()
        {
            read.alone = true;
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
            alone: true,
        }]
    }

    /// Whether the buffer holds only an ESC, which is Esc unless more bytes follow soon.
    pub fn lone_esc(&self) -> bool {
        self.buf == [ESC]
    }
}

const ESC: u8 = 0x1b;

/// The sequence at the head of `bytes`: how many bytes it takes and its event, `None` for a dropped one. A printable
/// key is what `keys` makes it. Any byte or whole CSI/SS3 sequence that is no other key is `Other` (v6f call 12).
/// `None` overall when the sequence is unfinished.
fn decode(bytes: &[u8], keys: &Keymap) -> Option<(usize, Option<Event>)> {
    let key = match bytes[0] {
        0x03 => Event::Interrupt,
        b'\r' | b'\n' => Event::Enter,
        b'y' => Event::Yes,
        b'\t' => Event::Tab,
        ESC => return escape(bytes),
        byte => keys.event(byte),
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
