//! Shift pressed twice, alone and quickly, as IntelliJ opens Search
//! Everywhere: a key typed between, another modifier held, or too long a
//! wait starts over.

use std::time::{Duration, Instant};

use gpui::Modifiers;

/// How soon the second press must follow the first.
pub const WITHIN: Duration = Duration::from_millis(350);

#[derive(Default)]
pub struct DoubleShift {
    /// When Shift was last pressed alone.
    pressed: Option<Instant>,
    /// Whether Shift is down now.
    down: bool,
}

impl DoubleShift {
    /// Takes the modifiers as they now are; true when this is the second
    /// press.
    pub fn modifiers(&mut self, m: &Modifiers, now: Instant) -> bool {
        let alone = m.shift && !m.control && !m.alt && !m.platform && !m.function;
        let was_down = std::mem::replace(&mut self.down, m.shift);
        if !alone {
            if m.control || m.alt || m.platform || m.function {
                self.pressed = None;
            }
            return false;
        }
        if was_down {
            return false;
        }
        match self.pressed {
            Some(t) if now.saturating_duration_since(t) <= WITHIN => {
                self.pressed = None;
                true
            }
            _ => {
                self.pressed = Some(now);
                false
            }
        }
    }

    /// A key typed: Shift held for a capital, or anything else, is not a
    /// double press.
    pub fn key(&mut self) {
        self.pressed = None;
    }
}
