//! Ghostty's terminal emulation, libghostty-vt (built by build.rs at the
//! release it pins), as Hangar's terminals use it: a terminal that is fed
//! what a shell writes and read back as rows of styled cells, with what the
//! program asks of the terminal -- replies, a title, a copy to the
//! clipboard, a notification, progress -- collected as effects to be taken.
//!
//! Positions are (row, column) from the top left of the viewport, the rows
//! shown, unless said otherwise. A Terminal and everything it hands out
//! stay on the thread that made it, as libghostty-vt requires.

#[allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    dead_code,
    unsafe_op_in_unsafe_fn,
    clippy::all
)]
mod sys;

use std::ffi::c_void;
use std::ptr;

use sys::GhosttyResult::{GHOSTTY_NO_VALUE, GHOSTTY_OUT_OF_SPACE, GHOSTTY_SUCCESS};

/// The version of Ghostty whose source libghostty-vt was built from
/// ("1.4.1-dev": its build.zig.zon's), which a terminal says it is.
pub fn ghostty_version() -> String {
    env!("HANGAR_GHOSTTY_VERSION").to_string()
}

/// libghostty-vt's own version ("1.0.1"), as its build says.
pub fn vt_version() -> String {
    let mut s = sys::GhosttyString {
        ptr: ptr::null(),
        len: 0,
    };
    // SAFETY: the build's version string is static.
    unsafe {
        let r = sys::ghostty_build_info(
            sys::GhosttyBuildInfo::GHOSTTY_BUILD_INFO_VERSION_STRING,
            &mut s as *mut _ as *mut c_void,
        );
        if r != GHOSTTY_SUCCESS {
            return String::new();
        }
        text(&s)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl From<sys::GhosttyColorRgb> for Rgb {
    fn from(c: sys::GhosttyColorRgb) -> Rgb {
        Rgb {
            r: c.r,
            g: c.g,
            b: c.b,
        }
    }
}

impl From<Rgb> for sys::GhosttyColorRgb {
    fn from(c: Rgb) -> sys::GhosttyColorRgb {
        sys::GhosttyColorRgb {
            r: c.r,
            g: c.g,
            b: c.b,
        }
    }
}

/// How wide a cell's character is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Wide {
    #[default]
    Narrow,
    /// The first of the two cells a wide character takes.
    Wide,
    /// The second, which holds nothing of its own.
    SpacerTail,
    /// A cell at the end of a row left empty for a wide character that
    /// wrapped to the next.
    SpacerHead,
}

/// One cell of the screen as it is drawn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cell {
    /// Its grapheme cluster; empty for a cell with no text.
    pub text: String,
    pub wide: Wide,
    /// Its colours, None where it has none of its own and takes the
    /// screen's.
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub underline: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
    /// Within the selection.
    pub selected: bool,
}

/// One row of the screen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    pub cells: Vec<Cell>,
}

/// The screen as it is to be drawn: its rows, its colours, the cursor, and
/// the pictures placed on it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frame {
    pub rows: Vec<Row>,
    pub fg: Rgb,
    pub bg: Rgb,
    /// Where the cursor is, if it is in the viewport, and whether the
    /// program shows it.
    pub cursor: Option<(usize, usize)>,
    pub cursor_visible: bool,
    /// The pictures on the viewport, in the order they are drawn.
    pub placements: Vec<Placement>,
}

/// Where a picture is drawn against the text: under the cells' own
/// backgrounds, between them and the text, or over the text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    BelowBackground,
    #[default]
    BelowText,
    AboveText,
}

/// A picture a program placed with kitty graphics, and where it is drawn:
/// from a cell of the viewport, offset into it and sized in pixels of the
/// cell size given to resize, showing a part of its image scaled to fit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Placement {
    /// The image shown (Terminal::picture), and the generation of its
    /// pixels, which changes whenever they do.
    pub image: u32,
    pub generation: u64,
    pub layer: Layer,
    /// Orders the pictures of a layer, the lowest drawn first.
    pub z: i32,
    /// The cell its top left is in, negative above or left of the
    /// viewport: a picture is placed whole, and clipped where it is drawn.
    pub row: i32,
    pub col: i32,
    /// How far into that cell it starts, in pixels.
    pub offset: (u32, u32),
    /// Its size drawn, in pixels.
    pub width: u32,
    pub height: u32,
    /// The part of the image shown, in its pixels: x, y, width, height.
    pub source: (u32, u32, u32, u32),
    /// One row's run of the cells of a picture a program shows with
    /// Unicode placeholders, which come a row at a time.
    pub strip: bool,
}

/// An image's pixels: RGBA, a byte a channel, row by row from the top left.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// What the program has asked to hear of the mouse.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mouse {
    #[default]
    None,
    /// Presses alone (9).
    X10,
    /// Presses and releases (1000).
    Normal,
    /// And moves while a button is held (1002).
    Button,
    /// And every move (1003).
    Any,
}

/// A notification the program asked for (OSC 9 or OSC 777).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
}

/// Progress the program reports (OSC 9;4): a percentage from 0 to 100
/// where it gives one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    Remove,
    Set(u8),
    Error(Option<u8>),
    Indeterminate,
    Pause(Option<u8>),
}

/// What the program did that the terminal's owner must act on, since it
/// was last taken.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Effects {
    /// What the terminal answers the program, to be written to it.
    pub reply: Vec<u8>,
    /// The title it set, the last one.
    pub title: Option<String>,
    /// The text it copied to the clipboard, the last.
    pub copied: Option<String>,
    pub notifications: Vec<Notification>,
    /// The progress it reported, the last.
    pub progress: Option<Progress>,
    /// The working directory it said it is in (OSC 7's URI as it is), the
    /// last.
    pub pwd: Option<String>,
    pub bell: bool,
}

/// What the callbacks write to, which the terminal holds a pointer to.
#[derive(Default)]
struct State {
    effects: Effects,
    /// The answer to XTVERSION.
    version: Vec<u8>,
    dark: bool,
    /// The terminal's size, for the size reports: rows, columns, and a
    /// cell's width and height in pixels.
    size: (u16, u16, u32, u32),
}

pub struct Terminal {
    raw: sys::GhosttyTerminal,
    render: sys::GhosttyRenderState,
    rows: sys::GhosttyRenderStateRowIterator,
    cells: sys::GhosttyRenderStateRowCells,
    images: sys::GhosttyRenderStateImageIterator,
    search: sys::GhosttySearch,
    state: Box<State>,
}

/// A failure of libghostty-vt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(pub i32);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "libghostty-vt failed ({})", self.0)
    }
}

impl std::error::Error for Error {}

fn check(r: sys::GhosttyResult::Type) -> Result<(), Error> {
    if r == GHOSTTY_SUCCESS {
        Ok(())
    } else {
        Err(Error(r))
    }
}

/// A sized struct, its size set as libghostty-vt reads it.
fn sized<T: Default>(set: impl FnOnce(&mut T) -> &mut usize) -> T {
    let mut v = T::default();
    *set(&mut v) = std::mem::size_of::<T>();
    v
}

impl Terminal {
    /// A terminal of columns by rows, which answers XTVERSION with version
    /// ("ghostty 1.4.0", say; empty is libghostty-vt's own answer).
    pub fn new(columns: u16, rows: u16, version: &str) -> Result<Terminal, Error> {
        static PNG: std::sync::Once = std::sync::Once::new();
        // SAFETY: the decoder is a plain function, set once for the
        // process before any terminal reads a picture.
        PNG.call_once(|| unsafe {
            sys::ghostty_sys_set(
                sys::GhosttySysOption::GHOSTTY_SYS_OPT_DECODE_PNG,
                decode_png as *const c_void,
            );
        });
        let mut t = Terminal {
            raw: ptr::null_mut(),
            render: ptr::null_mut(),
            rows: ptr::null_mut(),
            cells: ptr::null_mut(),
            images: ptr::null_mut(),
            search: ptr::null_mut(),
            state: Box::new(State {
                version: version.as_bytes().to_vec(),
                size: (rows, columns, 0, 0),
                ..State::default()
            }),
        };
        // SAFETY: each handle is made once, freed by Drop, and used only
        // from this thread; the state the callbacks write lives in a Box
        // that outlives the terminal.
        unsafe {
            check(sys::ghostty_terminal_new(
                ptr::null(),
                &mut t.raw,
                columns.max(1),
                rows.max(1),
            ))?;
            check(sys::ghostty_render_state_new(ptr::null(), &mut t.render))?;
            check(sys::ghostty_render_state_row_iterator_new(
                ptr::null(),
                &mut t.rows,
            ))?;
            check(sys::ghostty_render_state_row_cells_new(
                ptr::null(),
                &mut t.cells,
            ))?;
            check(sys::ghostty_render_state_image_iterator_new(
                ptr::null(),
                &mut t.images,
            ))?;
            check(sys::ghostty_search_new(ptr::null(), &mut t.search, t.raw))?;
            use sys::GhosttyTerminalOption::*;
            let state: *mut State = &mut *t.state;
            t.set(GHOSTTY_TERMINAL_OPT_USERDATA, state as *const c_void)?;
            t.set(GHOSTTY_TERMINAL_OPT_WRITE_PTY, write_pty as *const c_void)?;
            t.set(GHOSTTY_TERMINAL_OPT_BELL, bell as *const c_void)?;
            t.set(GHOSTTY_TERMINAL_OPT_XTVERSION, xtversion as *const c_void)?;
            t.set(
                GHOSTTY_TERMINAL_OPT_TITLE_CHANGED,
                title_changed as *const c_void,
            )?;
            t.set(
                GHOSTTY_TERMINAL_OPT_PWD_CHANGED,
                pwd_changed as *const c_void,
            )?;
            t.set(GHOSTTY_TERMINAL_OPT_SIZE, size as *const c_void)?;
            t.set(
                GHOSTTY_TERMINAL_OPT_COLOR_SCHEME,
                color_scheme as *const c_void,
            )?;
            t.set(
                GHOSTTY_TERMINAL_OPT_CLIPBOARD_WRITE,
                clipboard_write as *const c_void,
            )?;
            t.set(
                GHOSTTY_TERMINAL_OPT_DESKTOP_NOTIFICATION,
                notification as *const c_void,
            )?;
            t.set(
                GHOSTTY_TERMINAL_OPT_PROGRESS_REPORT,
                progress as *const c_void,
            )?;
        }
        Ok(t)
    }

    /// Sets an option to a value as libghostty-vt reads it.
    ///
    /// # Safety
    /// value must be what the option takes, valid for the call.
    unsafe fn set(
        &mut self,
        option: sys::GhosttyTerminalOption::Type,
        value: *const c_void,
    ) -> Result<(), Error> {
        check(unsafe { sys::ghostty_terminal_set(self.raw, option, value) })
    }

    /// Reads a datum into out.
    fn get<T: Default>(&self, data: sys::GhosttyTerminalData::Type) -> Option<T> {
        let mut out = T::default();
        // SAFETY: out is the type the datum is read as, at each call site.
        let r =
            unsafe { sys::ghostty_terminal_get(self.raw, data, &mut out as *mut T as *mut c_void) };
        (r == GHOSTTY_SUCCESS).then_some(out)
    }

    /// Takes what the shell wrote.
    pub fn feed(&mut self, bytes: &[u8]) {
        // SAFETY: the bytes are read for the call alone.
        unsafe { sys::ghostty_terminal_vt_write(self.raw, bytes.as_ptr(), bytes.len()) }
    }

    /// Sets the size, in cells and a cell's pixels.
    pub fn resize(&mut self, columns: u16, rows: u16, cell_width: u32, cell_height: u32) {
        let (columns, rows) = (columns.max(1), rows.max(1));
        self.state.size = (rows, columns, cell_width, cell_height);
        // SAFETY: a plain call on the terminal.
        let _ = unsafe {
            sys::ghostty_terminal_resize(self.raw, columns, rows, cell_width, cell_height)
        };
    }

    /// The effects since they were last taken.
    pub fn take_effects(&mut self) -> Effects {
        std::mem::take(&mut self.state.effects)
    }

    /// Sets the colours text and the background take when the program
    /// names none, and the 256 the palette's indices name.
    pub fn set_colours(&mut self, fg: Rgb, bg: Rgb, palette: &[Rgb; 256]) {
        use sys::GhosttyTerminalOption::*;
        let fg: sys::GhosttyColorRgb = fg.into();
        let bg: sys::GhosttyColorRgb = bg.into();
        let palette: [sys::GhosttyColorRgb; 256] = palette.map(Into::into);
        // SAFETY: each value is the option's type, valid for the call.
        unsafe {
            let _ = self.set(
                GHOSTTY_TERMINAL_OPT_COLOR_FOREGROUND,
                &fg as *const _ as *const c_void,
            );
            let _ = self.set(
                GHOSTTY_TERMINAL_OPT_COLOR_BACKGROUND,
                &bg as *const _ as *const c_void,
            );
            let _ = self.set(
                GHOSTTY_TERMINAL_OPT_COLOR_PALETTE,
                &palette as *const _ as *const c_void,
            );
        }
    }

    /// How many bytes of rows the scrollback keeps, the oldest dropped past
    /// it; None keeps all of it. Ghostty prunes whole pages of rows, so it
    /// keeps a little more.
    pub fn set_scrollback(&mut self, bytes: Option<usize>) {
        let bytes = bytes.unwrap_or(usize::MAX);
        // SAFETY: the option takes a pointer to a size_t, read in the call.
        unsafe {
            let _ = self.set(
                sys::GhosttyTerminalOption::GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES,
                &bytes as *const usize as *const c_void,
            );
        }
    }

    /// Whether the terminal is dark, as a program that asks is told
    /// (CSI ? 996 n).
    pub fn set_dark(&mut self, dark: bool) {
        self.state.dark = dark;
    }

    /// Scrolls the viewport through the scrollback by lines, up for
    /// positive.
    pub fn scroll(&mut self, lines: isize) {
        let mut how = sys::GhosttyTerminalScrollViewport {
            tag: sys::GhosttyTerminalScrollViewportTag::GHOSTTY_TERMINAL_SCROLL_VIEWPORT_DELTA,
            value: sys::GhosttyTerminalScrollViewportValue { _padding: [0; 2] },
        };
        how.value.delta = -lines;
        // SAFETY: a plain call on the terminal.
        let _ = unsafe { sys::ghostty_terminal_scroll_viewport(self.raw, how) };
    }

    /// Scrolls the viewport to the bottom, the screen the program draws on.
    pub fn scroll_to_bottom(&mut self) {
        let how = sys::GhosttyTerminalScrollViewport {
            tag: sys::GhosttyTerminalScrollViewportTag::GHOSTTY_TERMINAL_SCROLL_VIEWPORT_BOTTOM,
            value: sys::GhosttyTerminalScrollViewportValue { _padding: [0; 2] },
        };
        // SAFETY: a plain call on the terminal.
        let _ = unsafe { sys::ghostty_terminal_scroll_viewport(self.raw, how) };
    }

    /// The scrollable area, in rows: all of it, where the viewport's top
    /// is in it, and the viewport's length.
    fn scrollbar(&self) -> (u64, u64, u64) {
        let s: sys::GhosttyTerminalScrollbar = self
            .get(sys::GhosttyTerminalData::GHOSTTY_TERMINAL_DATA_SCROLLBAR)
            .unwrap_or_default();
        (s.total, s.offset, s.len)
    }

    /// How far the viewport is scrolled back from the bottom, in rows.
    pub fn scrolled_back(&self) -> usize {
        let (total, offset, len) = self.scrollbar();
        total.saturating_sub(offset + len) as usize
    }

    /// How many rows of scrollback are above the viewport.
    pub fn above_viewport(&self) -> usize {
        self.scrollbar().1 as usize
    }

    /// Whether a mode is set: a DEC private mode (?-prefixed), or with ansi
    /// an ANSI one.
    pub fn mode(&self, number: u16, ansi: bool) -> bool {
        let mut m = sys::GhosttyTerminalModeConfig {
            mode: (number & 0x7FFF) | ((ansi as u16) << 15),
            value: false,
        };
        // SAFETY: the mode config is the datum's type.
        let r = unsafe {
            sys::ghostty_terminal_get(
                self.raw,
                sys::GhosttyTerminalData::GHOSTTY_TERMINAL_DATA_MODE,
                &mut m as *mut _ as *mut c_void,
            )
        };
        r == GHOSTTY_SUCCESS && m.value
    }

    /// What the program has asked to hear of the mouse: the most it has
    /// asked of the four modes.
    pub fn mouse(&self) -> Mouse {
        if self.mode(1003, false) {
            Mouse::Any
        } else if self.mode(1002, false) {
            Mouse::Button
        } else if self.mode(1000, false) {
            Mouse::Normal
        } else if self.mode(9, false) {
            Mouse::X10
        } else {
            Mouse::None
        }
    }

    /// Where the cursor is on the screen the program draws on, as (row,
    /// column), and whether the program shows it.
    pub fn cursor(&self) -> ((usize, usize), bool) {
        use sys::GhosttyTerminalData::*;
        let x: u16 = self.get(GHOSTTY_TERMINAL_DATA_CURSOR_X).unwrap_or(0);
        let y: u16 = self.get(GHOSTTY_TERMINAL_DATA_CURSOR_Y).unwrap_or(0);
        let shown: bool = self
            .get(GHOSTTY_TERMINAL_DATA_CURSOR_VISIBLE)
            .unwrap_or(true);
        ((y as usize, x as usize), shown)
    }

    /// How many bytes of kitty graphics the terminal keeps; zero turns the
    /// protocol off, its queries unanswered.
    pub fn set_kitty_image_limit(&mut self, bytes: u64) {
        // SAFETY: the value is the option's type, valid for the call.
        unsafe {
            let _ = self.set(
                sys::GhosttyTerminalOption::GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT,
                &bytes as *const u64 as *const c_void,
            );
        }
    }

    /// Clears the scrollback and the screen above the cursor, the cursor's
    /// row going to the top. A program's alternate screen is left as it is
    /// and only its scrollback cleared.
    pub fn clear_buffer(&mut self) {
        self.clear_selection();
        self.scroll_to_bottom();
        let ((row, col), _) = self.cursor();
        if !self.alternate_screen() && row > 0 {
            self.feed(format!("\x1b[{row}S\x1b[1;{}H", col + 1).as_bytes());
        }
        self.feed(b"\x1b[3J");
    }

    /// The kitty keyboard protocol's flags the program has set.
    pub fn kitty_keyboard(&self) -> u8 {
        self.get(sys::GhosttyTerminalData::GHOSTTY_TERMINAL_DATA_KITTY_KEYBOARD_FLAGS)
            .unwrap_or(0)
    }

    /// Whether the program draws on the alternate screen.
    pub fn alternate_screen(&self) -> bool {
        let s: sys::GhosttyTerminalScreen::Type = self
            .get(sys::GhosttyTerminalData::GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN)
            .unwrap_or(0);
        s == sys::GhosttyTerminalScreen::GHOSTTY_TERMINAL_SCREEN_ALTERNATE
    }

    /// The screen as it is to be drawn.
    pub fn frame(&mut self) -> Frame {
        self.refresh_search();
        let mut frame = Frame::default();
        // SAFETY: the render state is read only between its update and
        // the next change to the terminal, here; every out value is the
        // type its datum is read as.
        unsafe {
            if sys::ghostty_render_state_update(self.render, self.raw) != GHOSTTY_SUCCESS {
                return frame;
            }
            use sys::GhosttyRenderStateData::*;
            let mut colours: sys::GhosttyRenderStateColors =
                sized(|c: &mut sys::GhosttyRenderStateColors| &mut c.size);
            sys::ghostty_render_state_get(
                self.render,
                GHOSTTY_RENDER_STATE_DATA_COLORS,
                &mut colours as *mut _ as *mut c_void,
            );
            frame.fg = colours.foreground.into();
            frame.bg = colours.background.into();
            let mut cursor: sys::GhosttyRenderStateCursor =
                sized(|c: &mut sys::GhosttyRenderStateCursor| &mut c.size);
            sys::ghostty_render_state_get(
                self.render,
                GHOSTTY_RENDER_STATE_DATA_CURSOR,
                &mut cursor as *mut _ as *mut c_void,
            );
            frame.cursor_visible = cursor.visible;
            if cursor.viewport_has_value {
                frame.cursor = Some((cursor.viewport_y as usize, cursor.viewport_x as usize));
            }
            sys::ghostty_render_state_get(
                self.render,
                GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR,
                &mut self.rows as *mut _ as *mut c_void,
            );
            let mut codepoints = [0u32; 32];
            while sys::ghostty_render_state_row_iterator_next(self.rows) {
                let mut row = Row::default();
                sys::ghostty_render_state_row_get(
                    self.rows,
                    sys::GhosttyRenderStateRowData::GHOSTTY_RENDER_STATE_ROW_DATA_CELLS,
                    &mut self.cells as *mut _ as *mut c_void,
                );
                while sys::ghostty_render_state_row_cells_next(self.cells) {
                    row.cells.push(self.cell(&mut codepoints));
                }
                frame.rows.push(row);
            }
            frame.placements = self.placements();
            sys::ghostty_render_state_clean(self.render);
        }
        frame
    }

    /// The pictures the render state found, in the order they are drawn.
    ///
    /// # Safety
    /// The render state must be just updated.
    unsafe fn placements(&mut self) -> Vec<Placement> {
        use sys::GhosttyRenderStateImageData::*;
        let mut out = vec![];
        let r = unsafe {
            sys::ghostty_render_state_get(
                self.render,
                sys::GhosttyRenderStateData::GHOSTTY_RENDER_STATE_DATA_IMAGE_ITERATOR,
                &mut self.images as *mut _ as *mut c_void,
            )
        };
        if r != GHOSTTY_SUCCESS {
            return out;
        }
        while unsafe { sys::ghostty_render_state_image_iterator_next(self.images) } {
            let images = self.images;
            let get = |data, out: *mut c_void| unsafe {
                sys::ghostty_render_state_image_get(images, data, out) == GHOSTTY_SUCCESS
            };
            let mut protocol: sys::GhosttyImageProtocol::Type = 0;
            if !get(
                GHOSTTY_RENDER_STATE_IMAGE_DATA_PROTOCOL,
                &mut protocol as *mut _ as *mut c_void,
            ) || protocol != sys::GhosttyImageProtocol::GHOSTTY_IMAGE_PROTOCOL_KITTY
            {
                continue;
            }
            let mut p = Placement::default();
            let mut layer: sys::GhosttyImageLayer::Type = 0;
            let (mut sx, mut sy, mut sw, mut sh) = (0u32, 0u32, 0u32, 0u32);
            let read = get(
                GHOSTTY_RENDER_STATE_IMAGE_DATA_IMAGE_ID,
                &mut p.image as *mut _ as *mut c_void,
            ) && get(
                GHOSTTY_RENDER_STATE_IMAGE_DATA_IMAGE_GENERATION,
                &mut p.generation as *mut _ as *mut c_void,
            ) && get(
                GHOSTTY_RENDER_STATE_IMAGE_DATA_LAYER,
                &mut layer as *mut _ as *mut c_void,
            ) && get(
                GHOSTTY_RENDER_STATE_IMAGE_DATA_Z,
                &mut p.z as *mut _ as *mut c_void,
            ) && get(
                GHOSTTY_RENDER_STATE_IMAGE_DATA_VIEWPORT_ROW,
                &mut p.row as *mut _ as *mut c_void,
            ) && get(
                GHOSTTY_RENDER_STATE_IMAGE_DATA_VIEWPORT_COL,
                &mut p.col as *mut _ as *mut c_void,
            ) && get(
                GHOSTTY_RENDER_STATE_IMAGE_DATA_CELL_OFFSET_X,
                &mut p.offset.0 as *mut _ as *mut c_void,
            ) && get(
                GHOSTTY_RENDER_STATE_IMAGE_DATA_CELL_OFFSET_Y,
                &mut p.offset.1 as *mut _ as *mut c_void,
            ) && get(
                GHOSTTY_RENDER_STATE_IMAGE_DATA_PIXEL_WIDTH,
                &mut p.width as *mut _ as *mut c_void,
            ) && get(
                GHOSTTY_RENDER_STATE_IMAGE_DATA_PIXEL_HEIGHT,
                &mut p.height as *mut _ as *mut c_void,
            ) && get(
                GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_X,
                &mut sx as *mut _ as *mut c_void,
            ) && get(
                GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_Y,
                &mut sy as *mut _ as *mut c_void,
            ) && get(
                GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_WIDTH,
                &mut sw as *mut _ as *mut c_void,
            ) && get(
                GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_HEIGHT,
                &mut sh as *mut _ as *mut c_void,
            );
            if !read {
                continue;
            }
            use sys::GhosttyImageLayer::*;
            p.layer = match layer {
                GHOSTTY_IMAGE_LAYER_BELOW_BACKGROUND => Layer::BelowBackground,
                GHOSTTY_IMAGE_LAYER_ABOVE_TEXT => Layer::AboveText,
                _ => Layer::BelowText,
            };
            p.source = (sx, sy, sw, sh);
            let mut kind: sys::GhosttyRenderStateKittyPlacementKind::Type =
                sys::GhosttyRenderStateKittyPlacementKind::GHOSTTY_RENDER_STATE_KITTY_PLACEMENT_KIND_FIXED;
            get(
                GHOSTTY_RENDER_STATE_IMAGE_DATA_KITTY_KIND,
                &mut kind as *mut _ as *mut c_void,
            );
            p.strip = kind
                == sys::GhosttyRenderStateKittyPlacementKind::GHOSTTY_RENDER_STATE_KITTY_PLACEMENT_KIND_VIRTUAL;
            out.push(p);
        }
        out.sort_by_key(|p| (p.layer, p.z));
        out
    }

    /// The kitty graphics storage of the screen shown.
    fn graphics(&self) -> Option<sys::GhosttyKittyGraphics> {
        let g: sys::GhosttyKittyGraphics = self
            .get(sys::GhosttyTerminalData::GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS)
            .unwrap_or(ptr::null_mut());
        (!g.is_null()).then_some(g)
    }

    /// An image's pixels, as its current frame has them, if its pixels have
    /// arrived.
    pub fn picture(&self, image: u32) -> Option<Picture> {
        use sys::GhosttyKittyGraphicsImageData::*;
        use sys::GhosttyKittyImageFormat::*;
        let graphics = self.graphics()?;
        // SAFETY: the image and its pixels are read before the terminal
        // next changes, and copied out.
        unsafe {
            let raw = sys::ghostty_kitty_graphics_image(graphics, image);
            if raw.is_null() {
                return None;
            }
            let get = |data, out: *mut c_void| {
                sys::ghostty_kitty_graphics_image_get(raw, data, out) == GHOSTTY_SUCCESS
            };
            let (mut width, mut height) = (0u32, 0u32);
            let mut format: sys::GhosttyKittyImageFormat::Type = 0;
            let mut pixels = sys::GhosttyString {
                ptr: ptr::null(),
                len: 0,
            };
            if !(get(
                GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_WIDTH,
                &mut width as *mut _ as *mut c_void,
            ) && get(
                GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_HEIGHT,
                &mut height as *mut _ as *mut c_void,
            ) && get(
                GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_FORMAT,
                &mut format as *mut _ as *mut c_void,
            ) && get(
                GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_PIXELS,
                &mut pixels as *mut _ as *mut c_void,
            )) || pixels.ptr.is_null()
            {
                return None;
            }
            let bytes = std::slice::from_raw_parts(pixels.ptr, pixels.len);
            let n = width as usize * height as usize;
            let rgba: Vec<u8> = match format {
                GHOSTTY_KITTY_IMAGE_FORMAT_RGBA => bytes.get(..n * 4)?.to_vec(),
                GHOSTTY_KITTY_IMAGE_FORMAT_RGB => bytes
                    .get(..n * 3)?
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .flat_map(|p| [p[0], p[1], p[2], 255])
                    .collect(),
                GHOSTTY_KITTY_IMAGE_FORMAT_GRAY_ALPHA => bytes
                    .get(..n * 2)?
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .flat_map(|p| [p[0], p[0], p[0], p[1]])
                    .collect(),
                GHOSTTY_KITTY_IMAGE_FORMAT_GRAY => bytes
                    .get(..n)?
                    .iter()
                    .flat_map(|&g| [g, g, g, 255])
                    .collect(),
                _ => return None,
            };
            Some(Picture {
                width,
                height,
                rgba,
            })
        }
    }

    /// Moves the pictures that are animations on to the frame due at now
    /// (milliseconds, on any clock that only goes forward), and says when
    /// the next is due on that clock, if one is playing.
    pub fn tick_animations(&mut self, now: u64) -> Option<u64> {
        let graphics = self.graphics()?;
        let mut next = 0u64;
        // SAFETY: a plain call on the terminal's storage.
        let r = unsafe { sys::ghostty_kitty_graphics_animation_tick(graphics, now, &mut next) };
        (r == GHOSTTY_SUCCESS).then_some(next)
    }

    /// The cell the cells iterator is on.
    ///
    /// # Safety
    /// The iterator must be on a cell, of a render state just updated.
    unsafe fn cell(&mut self, codepoints: &mut [u32; 32]) -> Cell {
        use sys::GhosttyRenderStateRowCellsData::*;
        let get = |data, out: *mut c_void| unsafe {
            sys::ghostty_render_state_row_cells_get(self.cells, data, out)
        };
        let mut cell = Cell::default();
        let mut buf = sys::GhosttyCodepointBuffer {
            ptr: codepoints.as_mut_ptr(),
            cap: codepoints.len(),
            len: 0,
        };
        let r = get(
            GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_BUF,
            &mut buf as *mut _ as *mut c_void,
        );
        if r == GHOSTTY_SUCCESS || r == GHOSTTY_OUT_OF_SPACE {
            cell.text = codepoints[..buf.len.min(buf.cap)]
                .iter()
                .filter_map(|&c| char::from_u32(c))
                .collect();
        }
        let mut raw: sys::GhosttyCell = 0;
        if get(
            GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW,
            &mut raw as *mut _ as *mut c_void,
        ) == GHOSTTY_SUCCESS
        {
            let mut wide: sys::GhosttyCellWide::Type = 0;
            unsafe {
                sys::ghostty_cell_get(
                    raw,
                    sys::GhosttyCellData::GHOSTTY_CELL_DATA_WIDE,
                    &mut wide as *mut _ as *mut c_void,
                )
            };
            use sys::GhosttyCellWide::*;
            cell.wide = match wide {
                GHOSTTY_CELL_WIDE_WIDE => Wide::Wide,
                GHOSTTY_CELL_WIDE_SPACER_TAIL => Wide::SpacerTail,
                GHOSTTY_CELL_WIDE_SPACER_HEAD => Wide::SpacerHead,
                _ => Wide::Narrow,
            };
        }
        let mut rgb = sys::GhosttyColorRgb { r: 0, g: 0, b: 0 };
        if get(
            GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_FG_COLOR,
            &mut rgb as *mut _ as *mut c_void,
        ) == GHOSTTY_SUCCESS
        {
            cell.fg = Some(rgb.into());
        }
        if get(
            GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR,
            &mut rgb as *mut _ as *mut c_void,
        ) == GHOSTTY_SUCCESS
        {
            cell.bg = Some(rgb.into());
        }
        let mut styled = false;
        get(
            GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_HAS_STYLING,
            &mut styled as *mut _ as *mut c_void,
        );
        if styled {
            let mut style = std::mem::MaybeUninit::<sys::GhosttyStyle>::zeroed();
            if get(
                GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE,
                style.as_mut_ptr() as *mut c_void,
            ) == GHOSTTY_SUCCESS
            {
                let f = unsafe { style.assume_init() }.flags;
                cell.bold = f & (1 << 0) != 0;
                cell.italic = f & (1 << 1) != 0;
                cell.faint = f & (1 << 2) != 0;
                cell.inverse = f & (1 << 4) != 0;
                cell.invisible = f & (1 << 5) != 0;
                cell.strikethrough = f & (1 << 6) != 0;
                cell.underline = (f >> 8) & 0x7 != 0;
            }
        }
        get(
            GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_SELECTED,
            &mut cell.selected as *mut _ as *mut c_void,
        );
        cell
    }

    /// A reference to the cell at a point, valid until the terminal next
    /// changes.
    fn grid_ref(
        &self,
        tag: sys::GhosttyPointTag::Type,
        row: u32,
        col: u16,
    ) -> Option<sys::GhosttyGridRef> {
        let point = sys::GhosttyPoint {
            tag,
            value: sys::GhosttyPointValue {
                coordinate: sys::GhosttyPointCoordinate { x: col, y: row },
            },
        };
        let mut out = sys::GhosttyGridRef {
            node: ptr::null_mut(),
            x: 0,
            y: 0,
        };
        // SAFETY: a plain call; out is written on success.
        let r = unsafe { sys::ghostty_terminal_grid_ref(self.raw, point, &mut out) };
        (r == GHOSTTY_SUCCESS).then_some(out)
    }

    /// A point of the viewport as a reference.
    fn at(&self, row: usize, col: usize) -> Option<sys::GhosttyGridRef> {
        self.grid_ref(
            sys::GhosttyPointTag::GHOSTTY_POINT_TAG_VIEWPORT,
            row as u32,
            col as u16,
        )
    }

    /// Where a reference is: in the viewport, as (row, column), or in the
    /// whole screen, scrollback and all, from its top.
    fn place(
        &self,
        r: &sys::GhosttyGridRef,
        tag: sys::GhosttyPointTag::Type,
    ) -> Option<(usize, usize)> {
        let mut out = sys::GhosttyPointCoordinate { x: 0, y: 0 };
        // SAFETY: r is a reference the terminal gave since it last changed.
        let res = unsafe { sys::ghostty_terminal_point_from_grid_ref(self.raw, r, tag, &mut out) };
        (res == GHOSTTY_SUCCESS).then_some((out.y as usize, out.x as usize))
    }

    /// A point of the viewport as a point of the whole screen, scrollback
    /// and all, from its top.
    pub fn screen_point(&self, row: usize, col: usize) -> Option<(usize, usize)> {
        let r = self.at(row, col)?;
        self.place(&r, sys::GhosttyPointTag::GHOSTTY_POINT_TAG_SCREEN)
    }

    /// A point of the whole screen as a reference.
    fn on_screen(&self, (row, col): (usize, usize)) -> Option<sys::GhosttyGridRef> {
        self.grid_ref(
            sys::GhosttyPointTag::GHOSTTY_POINT_TAG_SCREEN,
            row as u32,
            col as u16,
        )
    }

    /// Selects from one point of the whole screen to another, both
    /// included, as text runs.
    pub fn select(&mut self, from: (usize, usize), to: (usize, usize)) {
        let (Some(start), Some(end)) = (self.on_screen(from), self.on_screen(to)) else {
            return;
        };
        let s = sys::GhosttySelection {
            start,
            end,
            rectangle: false,
        };
        // SAFETY: the selection's references are fresh.
        unsafe {
            let _ = self.set(
                sys::GhosttyTerminalOption::GHOSTTY_TERMINAL_OPT_SELECTION,
                &s as *const _ as *const c_void,
            );
        }
    }

    /// The word at a point of the whole screen: its ends, both included.
    pub fn word_at(&self, at: (usize, usize)) -> Option<((usize, usize), (usize, usize))> {
        let r = self.on_screen(at)?;
        let mut opts: sys::GhosttyTerminalSelectWordOptions =
            sized(|o: &mut sys::GhosttyTerminalSelectWordOptions| &mut o.size);
        opts.ref_ = r;
        self.ends(|raw, out| unsafe { sys::ghostty_terminal_select_word(raw, &opts, out) })
    }

    /// The line at a point of the whole screen, wrapped rows and all: its
    /// ends, both included.
    pub fn line_at(&self, at: (usize, usize)) -> Option<((usize, usize), (usize, usize))> {
        let r = self.on_screen(at)?;
        let mut opts: sys::GhosttyTerminalSelectLineOptions =
            sized(|o: &mut sys::GhosttyTerminalSelectLineOptions| &mut o.size);
        opts.ref_ = r;
        self.ends(|raw, out| unsafe { sys::ghostty_terminal_select_line(raw, &opts, out) })
    }

    /// A selection's ends in the whole screen, in order.
    fn ends(
        &self,
        make: impl FnOnce(sys::GhosttyTerminal, *mut sys::GhosttySelection) -> sys::GhosttyResult::Type,
    ) -> Option<((usize, usize), (usize, usize))> {
        let mut s = std::mem::MaybeUninit::<sys::GhosttySelection>::zeroed();
        if make(self.raw, s.as_mut_ptr()) != GHOSTTY_SUCCESS {
            return None;
        }
        // SAFETY: written on success.
        let s = unsafe { s.assume_init() };
        let tag = sys::GhosttyPointTag::GHOSTTY_POINT_TAG_SCREEN;
        let (a, b) = (self.place(&s.start, tag)?, self.place(&s.end, tag)?);
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    /// Selects everything, the scrollback and the screen.
    pub fn select_all(&mut self) {
        if let Some((from, to)) =
            self.ends(|raw, out| unsafe { sys::ghostty_terminal_select_all(raw, out) })
        {
            self.select(from, to);
        }
    }

    pub fn clear_selection(&mut self) {
        // SAFETY: NULL clears the selection.
        unsafe {
            let _ = self.set(
                sys::GhosttyTerminalOption::GHOSTTY_TERMINAL_OPT_SELECTION,
                ptr::null(),
            );
        }
    }

    /// The selected text, unwrapped rows joined; None with nothing
    /// selected.
    pub fn selection(&self) -> Option<String> {
        let mut s = std::mem::MaybeUninit::<sys::GhosttySelection>::zeroed();
        // SAFETY: the selection is read into s, and formatted at once,
        // before the terminal changes.
        unsafe {
            let r = sys::ghostty_terminal_get(
                self.raw,
                sys::GhosttyTerminalData::GHOSTTY_TERMINAL_DATA_SELECTION,
                s.as_mut_ptr() as *mut c_void,
            );
            if r != GHOSTTY_SUCCESS {
                return None;
            }
            let s = s.assume_init();
            let text = self.format(Some(&s))?;
            (!text.is_empty()).then_some(text)
        }
    }

    /// The screen's text as plain text, or a selection's.
    ///
    /// # Safety
    /// The selection's references must be fresh.
    unsafe fn format(&self, selection: Option<&sys::GhosttySelection>) -> Option<String> {
        use sys::GhosttyFormatterOption::*;
        let mut f: sys::GhosttyFormatter = ptr::null_mut();
        unsafe {
            if sys::ghostty_formatter_terminal_new(ptr::null(), &mut f, self.raw) != GHOSTTY_SUCCESS
            {
                return None;
            }
            let unwrap = true;
            let trim = true;
            sys::ghostty_formatter_set(
                f,
                GHOSTTY_FORMATTER_OPT_UNWRAP,
                &unwrap as *const _ as *const c_void,
            );
            sys::ghostty_formatter_set(
                f,
                GHOSTTY_FORMATTER_OPT_TRIM,
                &trim as *const _ as *const c_void,
            );
            if let Some(s) = selection {
                sys::ghostty_formatter_set(
                    f,
                    GHOSTTY_FORMATTER_OPT_SELECTION,
                    s as *const _ as *const c_void,
                );
            }
            let (mut p, mut len) = (ptr::null_mut(), 0usize);
            let r = sys::ghostty_formatter_format_alloc(f, ptr::null(), &mut p, &mut len);
            sys::ghostty_formatter_free(f);
            if r != GHOSTTY_SUCCESS {
                return None;
            }
            let text = if p.is_null() {
                String::new()
            } else {
                String::from_utf8_lossy(std::slice::from_raw_parts(p, len)).into_owned()
            };
            if !p.is_null() {
                sys::ghostty_free(ptr::null(), p, len);
            }
            Some(text)
        }
    }

    /// The URI of the link a program wrote around the text (OSC 8) at a
    /// point of the viewport, if there is one.
    pub fn hyperlink_at(&self, row: usize, col: usize) -> Option<String> {
        let r = self.at(row, col)?;
        self.hyperlink(&r)
    }

    /// The URI of the link at a cell.
    fn hyperlink(&self, r: &sys::GhosttyGridRef) -> Option<String> {
        // SAFETY: r is fresh; the URI is copied into buf, sized first.
        unsafe {
            let mut cell: sys::GhosttyCell = 0;
            if sys::ghostty_grid_ref_cell(r, &mut cell) != GHOSTTY_SUCCESS {
                return None;
            }
            let mut has = false;
            sys::ghostty_cell_get(
                cell,
                sys::GhosttyCellData::GHOSTTY_CELL_DATA_HAS_HYPERLINK,
                &mut has as *mut _ as *mut c_void,
            );
            if !has {
                return None;
            }
            let mut len = 0usize;
            let res = sys::ghostty_grid_ref_hyperlink_uri(r, ptr::null_mut(), 0, &mut len);
            if res != GHOSTTY_OUT_OF_SPACE && res != GHOSTTY_SUCCESS {
                return None;
            }
            let mut buf = vec![0u8; len];
            if sys::ghostty_grid_ref_hyperlink_uri(r, buf.as_mut_ptr(), buf.len(), &mut len)
                != GHOSTTY_SUCCESS
            {
                return None;
            }
            buf.truncate(len);
            String::from_utf8(buf).ok()
        }
    }

    /// The cells carrying a link whose URI starts with prefix, in the
    /// viewport and the rows above it given: each's row (above the
    /// viewport negative), column and the rest of its URI.
    pub fn links_named(
        &self,
        prefix: &str,
        above: usize,
        columns: usize,
        rows: usize,
    ) -> Vec<(i32, usize, String)> {
        let top = self.above_viewport();
        let first = top.saturating_sub(above);
        let mut out = vec![];
        for y in first..top + rows {
            for x in 0..columns {
                let Some(r) = self.on_screen((y, x)) else {
                    continue;
                };
                if let Some(rest) = self
                    .hyperlink(&r)
                    .and_then(|u| u.strip_prefix(prefix).map(str::to_string))
                {
                    out.push((y as i32 - top as i32, x, rest));
                }
            }
        }
        out
    }

    /// Whether a row of the viewport continues on the next, the line
    /// having wrapped.
    pub fn wraps(&self, row: usize) -> bool {
        let Some(r) = self.at(row, 0) else {
            return false;
        };
        // SAFETY: r is fresh.
        unsafe {
            let mut raw: sys::GhosttyRow = 0;
            if sys::ghostty_grid_ref_row(&r, &mut raw) != GHOSTTY_SUCCESS {
                return false;
            }
            let mut wrap = false;
            sys::ghostty_row_get(
                raw,
                sys::GhosttyRowData::GHOSTTY_ROW_DATA_WRAP,
                &mut wrap as *mut _ as *mut c_void,
            );
            wrap
        }
    }

    /// Finds text, ASCII letters in any case, and scrolls to it if it is
    /// not in view. A new query finds the newest match, the lowest;
    /// asked again, older goes to the one above it and else the one below,
    /// wrapping round. False when it is nowhere; an empty query ends
    /// finding.
    pub fn find(&mut self, query: &str, older: bool) -> bool {
        use sys::GhosttySearchOption::*;
        let fresh = self.needle().as_deref() != Some(query);
        // SAFETY: the needle is copied by the call.
        unsafe {
            if query.is_empty() {
                sys::ghostty_search_set(self.search, GHOSTTY_SEARCH_OPT_NEEDLE, ptr::null());
                return false;
            }
            if fresh {
                let s = sys::GhosttyString {
                    ptr: query.as_ptr(),
                    len: query.len(),
                };
                sys::ghostty_search_set(
                    self.search,
                    GHOSTTY_SEARCH_OPT_NEEDLE,
                    &s as *const _ as *const c_void,
                );
            }
            self.refresh_search();
            if fresh || older {
                sys::ghostty_search_select_next(self.search);
            } else {
                sys::ghostty_search_select_prev(self.search);
            }
        }
        self.found().is_some()
    }

    /// The text being found.
    fn needle(&self) -> Option<String> {
        let mut s = sys::GhosttyString {
            ptr: ptr::null(),
            len: 0,
        };
        // SAFETY: the needle's bytes are copied at once.
        unsafe {
            let r = sys::ghostty_search_get(
                self.search,
                sys::GhosttySearchData::GHOSTTY_SEARCH_DATA_NEEDLE,
                &mut s as *mut _ as *mut c_void,
            );
            if r != GHOSTTY_SUCCESS || s.ptr.is_null() {
                return None;
            }
            Some(String::from_utf8_lossy(std::slice::from_raw_parts(s.ptr, s.len)).into_owned())
        }
    }

    /// Brings the search up to date with the terminal.
    fn refresh_search(&mut self) {
        use sys::GhosttySearchStatus::*;
        // SAFETY: plain calls on the search, which reads the terminal.
        unsafe {
            for _ in 0..10_000 {
                let mut status = GHOSTTY_SEARCH_STATUS_COMPLETE;
                if sys::ghostty_search_tick(self.search, &mut status) != GHOSTTY_SUCCESS {
                    return;
                }
                match status {
                    GHOSTTY_SEARCH_STATUS_FEED_REQUIRED => {
                        sys::ghostty_search_feed(self.search);
                    }
                    GHOSTTY_SEARCH_STATUS_RUNNING => {}
                    _ => return,
                }
            }
        }
    }

    /// The match finding is on: its ends in the viewport, if it is in it.
    pub fn found(&self) -> Option<((usize, usize), (usize, usize))> {
        let mut s = std::mem::MaybeUninit::<sys::GhosttySelection>::zeroed();
        // SAFETY: the match is read into s and placed at once.
        unsafe {
            let r = sys::ghostty_search_get(
                self.search,
                sys::GhosttySearchData::GHOSTTY_SEARCH_DATA_SELECTED_MATCH,
                s.as_mut_ptr() as *mut c_void,
            );
            if r != GHOSTTY_SUCCESS {
                return None;
            }
            self.in_view(&s.assume_init())
        }
    }

    /// Every match in the viewport, its ends in it.
    pub fn matches(&self) -> Vec<((usize, usize), (usize, usize))> {
        // SAFETY: the matches are read into a buffer sized first, and
        // placed at once.
        unsafe {
            let mut buf = sys::GhosttySelectionBuffer {
                ptr: ptr::null_mut(),
                cap: 0,
                len: 0,
            };
            let data = sys::GhosttySearchData::GHOSTTY_SEARCH_DATA_VIEWPORT_MATCHES;
            let r = sys::ghostty_search_get(self.search, data, &mut buf as *mut _ as *mut c_void);
            if (r != GHOSTTY_OUT_OF_SPACE && r != GHOSTTY_SUCCESS) || buf.len == 0 {
                return vec![];
            }
            let mut v: Vec<sys::GhosttySelection> = Vec::with_capacity(buf.len);
            buf.ptr = v.as_mut_ptr();
            buf.cap = v.capacity();
            if sys::ghostty_search_get(self.search, data, &mut buf as *mut _ as *mut c_void)
                != GHOSTTY_SUCCESS
            {
                return vec![];
            }
            v.set_len(buf.len.min(buf.cap));
            v.iter().filter_map(|s| self.in_view(s)).collect()
        }
    }

    /// A selection's ends in the viewport, in order, if it is in it.
    fn in_view(&self, s: &sys::GhosttySelection) -> Option<((usize, usize), (usize, usize))> {
        let tag = sys::GhosttyPointTag::GHOSTTY_POINT_TAG_VIEWPORT;
        let (a, b) = (self.place(&s.start, tag)?, self.place(&s.end, tag)?);
        Some(if a <= b { (a, b) } else { (b, a) })
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // SAFETY: each handle was made by Terminal::new and is freed once;
        // the search goes before the terminal it holds state in.
        unsafe {
            sys::ghostty_search_free(self.search);
            sys::ghostty_render_state_row_cells_free(self.cells);
            sys::ghostty_render_state_image_iterator_free(self.images);
            sys::ghostty_render_state_row_iterator_free(self.rows);
            sys::ghostty_render_state_free(self.render);
            sys::ghostty_terminal_free(self.raw);
        }
    }
}

/// The state behind a callback's userdata.
///
/// # Safety
/// userdata must be the terminal's State, as Terminal::new sets it.
unsafe fn state<'a>(userdata: *mut c_void) -> &'a mut State {
    unsafe { &mut *(userdata as *mut State) }
}

/// A borrowed string as Rust's.
///
/// # Safety
/// The string must be valid for the call.
unsafe fn text(s: &sys::GhosttyString) -> String {
    if s.ptr.is_null() || s.len == 0 {
        return String::new();
    }
    String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(s.ptr, s.len) }).into_owned()
}

unsafe extern "C" fn write_pty(
    _: sys::GhosttyTerminal,
    userdata: *mut c_void,
    data: *const u8,
    len: usize,
) {
    if data.is_null() {
        return;
    }
    let s = unsafe { state(userdata) };
    s.effects
        .reply
        .extend_from_slice(unsafe { std::slice::from_raw_parts(data, len) });
}

unsafe extern "C" fn bell(_: sys::GhosttyTerminal, userdata: *mut c_void) {
    unsafe { state(userdata) }.effects.bell = true;
}

unsafe extern "C" fn xtversion(
    _: sys::GhosttyTerminal,
    userdata: *mut c_void,
) -> sys::GhosttyString {
    let s = unsafe { state(userdata) };
    sys::GhosttyString {
        ptr: s.version.as_ptr(),
        len: s.version.len(),
    }
}

/// A string datum of the terminal.
///
/// # Safety
/// The datum must be a GhosttyString one.
unsafe fn string(t: sys::GhosttyTerminal, data: sys::GhosttyTerminalData::Type) -> Option<String> {
    let mut s = sys::GhosttyString {
        ptr: ptr::null(),
        len: 0,
    };
    let r = unsafe { sys::ghostty_terminal_get(t, data, &mut s as *mut _ as *mut c_void) };
    (r == GHOSTTY_SUCCESS || r == GHOSTTY_NO_VALUE).then(|| unsafe { text(&s) })
}

unsafe extern "C" fn title_changed(t: sys::GhosttyTerminal, userdata: *mut c_void) {
    let title = unsafe { string(t, sys::GhosttyTerminalData::GHOSTTY_TERMINAL_DATA_TITLE) };
    unsafe { state(userdata) }.effects.title = title;
}

unsafe extern "C" fn pwd_changed(t: sys::GhosttyTerminal, userdata: *mut c_void) {
    let pwd = unsafe { string(t, sys::GhosttyTerminalData::GHOSTTY_TERMINAL_DATA_PWD) };
    unsafe { state(userdata) }.effects.pwd = pwd;
}

unsafe extern "C" fn size(
    _: sys::GhosttyTerminal,
    userdata: *mut c_void,
    out: *mut sys::GhosttySizeReportSize,
) -> bool {
    let (rows, columns, cell_width, cell_height) = unsafe { state(userdata) }.size;
    if out.is_null() || cell_width == 0 {
        return false;
    }
    unsafe {
        *out = sys::GhosttySizeReportSize {
            rows,
            columns,
            cell_width,
            cell_height,
        }
    };
    true
}

unsafe extern "C" fn color_scheme(
    _: sys::GhosttyTerminal,
    userdata: *mut c_void,
    out: *mut sys::GhosttyColorScheme::Type,
) -> bool {
    if out.is_null() {
        return false;
    }
    let dark = unsafe { state(userdata) }.dark;
    unsafe {
        *out = if dark {
            sys::GhosttyColorScheme::GHOSTTY_COLOR_SCHEME_DARK
        } else {
            sys::GhosttyColorScheme::GHOSTTY_COLOR_SCHEME_LIGHT
        }
    };
    true
}

/// A copy to the clipboard: the standard one's text is taken; the
/// others are refused, as a Mac has no primary selection.
unsafe extern "C" fn clipboard_write(
    _: sys::GhosttyTerminal,
    userdata: *mut c_void,
    write: *const sys::GhosttyClipboardWrite,
) {
    let Some(w) = (unsafe { write.as_ref() }) else {
        return;
    };
    use sys::GhosttyClipboardWriteResult::*;
    let mut result = GHOSTTY_CLIPBOARD_WRITE_RESULT_UNSUPPORTED;
    if w.location == sys::GhosttyClipboardLocation::GHOSTTY_CLIPBOARD_LOCATION_STANDARD
        && !w.contents.is_null()
    {
        let contents = unsafe { std::slice::from_raw_parts(w.contents, w.contents_len) };
        if let Some(c) = contents
            .iter()
            .find(|c| unsafe { text(&c.mime) }.starts_with("text/plain"))
        {
            unsafe { state(userdata) }.effects.copied = Some(unsafe { text(&c.data) });
            result = GHOSTTY_CLIPBOARD_WRITE_RESULT_SUCCESS;
        }
    }
    let reply = sys::GhosttyClipboardWriteReply {
        size: std::mem::size_of::<sys::GhosttyClipboardWriteReply>(),
        result,
        remember: false,
    };
    if let Some(answer) = w.reply {
        unsafe { answer(w, &reply) };
    }
}

unsafe extern "C" fn notification(
    _: sys::GhosttyTerminal,
    userdata: *mut c_void,
    n: *const sys::GhosttyTerminalDesktopNotification,
) {
    let Some(n) = (unsafe { n.as_ref() }) else {
        return;
    };
    let note = Notification {
        title: unsafe { text(&n.title) },
        body: unsafe { text(&n.body) },
    };
    unsafe { state(userdata) }.effects.notifications.push(note);
}

unsafe extern "C" fn progress(
    _: sys::GhosttyTerminal,
    userdata: *mut c_void,
    p: *const sys::GhosttyTerminalProgressReport,
) {
    let Some(p) = (unsafe { p.as_ref() }) else {
        return;
    };
    use sys::GhosttyTerminalProgressState::*;
    let pct = (p.progress >= 0).then_some(p.progress.clamp(0, 100) as u8);
    let report = match p.state {
        GHOSTTY_TERMINAL_PROGRESS_STATE_SET => Progress::Set(pct.unwrap_or(0)),
        GHOSTTY_TERMINAL_PROGRESS_STATE_ERROR => Progress::Error(pct),
        GHOSTTY_TERMINAL_PROGRESS_STATE_INDETERMINATE => Progress::Indeterminate,
        GHOSTTY_TERMINAL_PROGRESS_STATE_PAUSE => Progress::Pause(pct),
        _ => Progress::Remove,
    };
    unsafe { state(userdata) }.effects.progress = Some(report);
}

/// Decodes a PNG a program sent as kitty graphics, into pixels the
/// allocator given holds (RGBA, a byte a channel).
unsafe extern "C" fn decode_png(
    _: *mut c_void,
    allocator: *const sys::GhosttyAllocator,
    data: *const u8,
    len: usize,
    out: *mut sys::GhosttySysImage,
) -> bool {
    if data.is_null() || len == 0 || out.is_null() {
        return false;
    }
    // SAFETY: the data is the library's, valid for the call.
    let bytes = unsafe { std::slice::from_raw_parts(data, len) };
    let Ok(Some((width, height, rgba))) = std::panic::catch_unwind(|| png_rgba(bytes)) else {
        return false;
    };
    // SAFETY: the buffer is the allocator's, as long as the pixels, and
    // the library takes it with out.
    unsafe {
        let buf = sys::ghostty_alloc(allocator, rgba.len());
        if buf.is_null() {
            return false;
        }
        ptr::copy_nonoverlapping(rgba.as_ptr(), buf, rgba.len());
        *out = sys::GhosttySysImage {
            width,
            height,
            data: buf,
            data_len: rgba.len(),
        };
    }
    true
}

/// A PNG's first frame as RGBA.
fn png_rgba(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let px = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => px.to_vec(),
        png::ColorType::Rgb => px
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => px
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => px.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return None,
    };
    Some((info.width, info.height, rgba))
}
