//! What the terminal sends a program for a key or the mouse, as the program
//! has asked: xterm's encodings, the cursor keys' application mode, the
//! Kitty keyboard protocol's escape codes, and the mouse reports in xterm's
//! normal or SGR form.

/// A key pressed: its name as gpui gives it ("enter", "a", "f5"), the text
/// it types, if any, and the modifiers held.
#[derive(Clone, Copy, Debug, Default)]
pub struct Key<'a> {
    pub name: &'a str,
    pub text: Option<&'a str>,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

/// What the program has asked of the keyboard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyModes {
    /// The cursor keys' application sequences (ESC O A, not ESC [ A).
    pub app_cursor: bool,
    /// Kitty's disambiguate flag: keys whose legacy form is ambiguous
    /// (Escape, and keys with Ctrl or Alt, and Enter, Tab and Backspace
    /// with a modifier) are sent as CSI u.
    pub disambiguate: bool,
    /// Kitty's report-all flag: every key is sent as an escape code, text
    /// keys among them.
    pub all_as_escapes: bool,
    /// Whether Alt (Option on a Mac) sends ESC before a key's text.
    pub alt_is_meta: bool,
}

/// xterm's modifier parameter: 1 plus Shift 1, Alt 2, Ctrl 4.
fn modifiers(k: &Key) -> u8 {
    1 + k.shift as u8 + 2 * k.alt as u8 + 4 * k.ctrl as u8
}

/// A key's Kitty code: its Unicode code point unshifted, or the protocol's
/// own for Escape, Enter, Tab and Backspace.
fn kitty_code(k: &Key) -> Option<u32> {
    Some(match k.name {
        "escape" => 27,
        "enter" => 13,
        "tab" => 9,
        "backspace" => 127,
        "space" => 32,
        name if name.chars().count() == 1 => name.chars().next()?.to_ascii_lowercase() as u32,
        _ => return None,
    })
}

/// What a key sends; None for a key the terminal leaves to the window.
pub fn encode(k: &Key, m: &KeyModes) -> Option<Vec<u8>> {
    let mods = modifiers(k);
    let csi = |s: String| Some(format!("\x1b[{s}").into_bytes());
    // Keys with a letter and modifiers: arrows, Home and End, F1 to F4, as
    // CSI 1;mods X; with no modifiers, SS3 in application mode for arrows.
    let lettered = |c: char, ss3: bool| {
        if mods > 1 {
            csi(format!("1;{mods}{c}"))
        } else if ss3 {
            Some(format!("\x1bO{c}").into_bytes())
        } else {
            csi(c.to_string())
        }
    };
    // Keys with a number: CSI n~, or CSI n;mods~.
    let tilde = |n: u8| {
        if mods > 1 {
            csi(format!("{n};{mods}~"))
        } else {
            csi(format!("{n}~"))
        }
    };
    match k.name {
        "up" => return lettered('A', m.app_cursor),
        "down" => return lettered('B', m.app_cursor),
        "right" => return lettered('C', m.app_cursor),
        "left" => return lettered('D', m.app_cursor),
        "home" => return lettered('H', m.app_cursor),
        "end" => return lettered('F', m.app_cursor),
        "f1" => return lettered('P', true),
        "f2" => return lettered('Q', true),
        "f3" => return lettered('R', true),
        "f4" => return lettered('S', true),
        "insert" => return tilde(2),
        "delete" => return tilde(3),
        "pageup" => return tilde(5),
        "pagedown" => return tilde(6),
        "f5" => return tilde(15),
        "f6" => return tilde(17),
        "f7" => return tilde(18),
        "f8" => return tilde(19),
        "f9" => return tilde(20),
        "f10" => return tilde(21),
        "f11" => return tilde(23),
        "f12" => return tilde(24),
        _ => {}
    }
    let code = kitty_code(k);
    let special = matches!(k.name, "escape" | "enter" | "tab" | "backspace");
    // Kitty's escape codes, where the program asked and the key's legacy
    // form would not say it.
    let kitty = m.all_as_escapes
        || (m.disambiguate && (k.name == "escape" || k.ctrl || k.alt || (special && mods > 1)));
    if kitty && let Some(code) = code {
        return if mods > 1 {
            csi(format!("{code};{mods}u"))
        } else {
            csi(format!("{code}u"))
        };
    }
    let legacy: Option<&[u8]> = match k.name {
        "enter" => Some(b"\r"),
        "tab" if k.shift => Some(b"\x1b[Z"),
        "tab" => Some(b"\t"),
        "escape" => Some(b"\x1b"),
        "backspace" if k.alt => Some(b"\x1b\x7f"),
        "backspace" if k.ctrl => Some(b"\x08"),
        "backspace" => Some(b"\x7f"),
        "space" if k.ctrl => Some(b"\0"),
        _ => None,
    };
    if let Some(b) = legacy {
        return Some(b.to_vec());
    }
    if k.ctrl && k.name.len() == 1 {
        let c = k.name.as_bytes()[0].to_ascii_lowercase();
        let byte = match c {
            b'a'..=b'z' => Some(c & 0x1f),
            b'[' => Some(0x1b),
            b'\\' => Some(0x1c),
            b']' => Some(0x1d),
            b'/' => Some(0x1f),
            _ => None,
        };
        if let Some(b) = byte {
            return Some(if k.alt { vec![0x1b, b] } else { vec![b] });
        }
    }
    let text = k.text?;
    if k.alt && m.alt_is_meta {
        let mut b = vec![0x1b];
        b.extend_from_slice(text.as_bytes());
        return Some(b);
    }
    Some(text.as_bytes().to_vec())
}

/// A mouse button, or the wheel, as a program is told of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
    /// The pointer moving with no button down.
    None,
    WheelUp,
    WheelDown,
}

/// What happened with the button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    Down,
    Up,
    /// The pointer moving, with the button held (or none).
    Moved,
}

/// What the program has asked of the mouse.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MouseModes {
    /// Presses and releases (1000).
    pub clicks: bool,
    /// Moves with a button held, too (1002).
    pub drags: bool,
    /// Every move (1003).
    pub motion: bool,
    /// Reports as CSI < b;x;y M/m (1006), not the 8-bit form.
    pub sgr: bool,
}

impl MouseModes {
    /// Whether the program wants the mouse at all.
    pub fn any(&self) -> bool {
        self.clicks || self.drags || self.motion
    }
}

/// The report of a mouse event at a cell (0-based), with the modifiers
/// held; None when the program did not ask for this kind of event, or the
/// cell is beyond what the 8-bit form can say.
#[allow(clippy::too_many_arguments)]
pub fn mouse(
    button: Button,
    press: Press,
    col: usize,
    row: usize,
    shift: bool,
    alt: bool,
    ctrl: bool,
    m: &MouseModes,
) -> Option<Vec<u8>> {
    if !m.any() {
        return None;
    }
    let held = button != Button::None;
    if press == Press::Moved && !(m.motion || (m.drags && held)) {
        return None;
    }
    let mut b: u32 = match button {
        Button::Left => 0,
        Button::Middle => 1,
        Button::Right => 2,
        Button::None => 3,
        Button::WheelUp => 64,
        Button::WheelDown => 65,
    };
    if press == Press::Moved {
        b += 32;
    }
    b += 4 * shift as u32 + 8 * alt as u32 + 16 * ctrl as u32;
    let (x, y) = (col + 1, row + 1);
    if m.sgr {
        let end = if press == Press::Up { 'm' } else { 'M' };
        return Some(format!("\x1b[<{b};{x};{y}{end}").into_bytes());
    }
    // The 8-bit form says a release with button 3, and cannot reach past
    // column or row 223.
    if press == Press::Up {
        b = (b & !3) | 3;
    }
    if x > 223 || y > 223 {
        return None;
    }
    Some(vec![
        0x1b,
        b'[',
        b'M',
        32 + b as u8,
        32 + x as u8,
        32 + y as u8,
    ])
}
