//! The side bar's terminal input as events: keys and SGR mouse reports, decoded from the bytes `watch` reads.
#![allow(dead_code)] // Until T9 gives every item a caller (#61).

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
