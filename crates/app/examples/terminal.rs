//! A shell in a window: the terminal spike. A PTY runs $SHELL, its output is
//! parsed into the screen (hangar_app::terminal) and drawn as styled text,
//! and keys are written back. It prints how long each frame's lines take to
//! build and draw, every two seconds.
//!
//!   cargo run --release -p hangar-app --example terminal [COMMAND]
//!
//! COMMAND, if given, is typed into the shell as it starts. Unix only:
//! Windows's PTY (ConPTY) is reached otherwise.

#[cfg(unix)]
mod unix {
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    use alacritty_terminal::event::WindowSize;
    use alacritty_terminal::tty::{self, Options, Shell};
    use gpui::{
        App, Bounds, Context, FocusHandle, Font, Hsla, KeyDownEvent, Rgba, StyledText, TextRun,
        UnderlineStyle, Window, WindowBounds, WindowOptions, div, font, prelude::*, px, size,
    };
    use gpui_platform::application;
    use hangar_app::terminal::Screen;

    const COLUMNS: u16 = 120;
    const LINES: u16 = 36;

    struct Terminal {
        screen: Screen,
        pty: std::fs::File,
        focus: FocusHandle,
        font: Font,
        frames: u32,
        spent: Duration,
        since: Instant,
    }

    fn hsla(c: alacritty_terminal::vte::ansi::Rgb) -> Hsla {
        Rgba {
            r: c.r as f32 / 255.,
            g: c.g as f32 / 255.,
            b: c.b as f32 / 255.,
            a: 1.,
        }
        .into()
    }

    /// What a key sends to the shell.
    fn bytes(e: &KeyDownEvent) -> Option<Vec<u8>> {
        let k = &e.keystroke;
        let named: &[u8] = match k.key.as_str() {
            "enter" => b"\r",
            "backspace" => b"\x7f",
            "tab" => b"\t",
            "escape" => b"\x1b",
            "up" => b"\x1b[A",
            "down" => b"\x1b[B",
            "right" => b"\x1b[C",
            "left" => b"\x1b[D",
            "home" => b"\x1b[H",
            "end" => b"\x1b[F",
            "pageup" => b"\x1b[5~",
            "pagedown" => b"\x1b[6~",
            "delete" => b"\x1b[3~",
            _ => b"",
        };
        if !named.is_empty() {
            return Some(named.to_vec());
        }
        if k.modifiers.control && k.key.len() == 1 {
            let c = k.key.as_bytes()[0].to_ascii_lowercase();
            if c.is_ascii_lowercase() {
                return Some(vec![c & 0x1f]);
            }
        }
        k.key_char.as_ref().map(|s| s.as_bytes().to_vec())
    }

    impl Render for Terminal {
        fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let start = Instant::now();
            let lines = self.screen.lines();
            let (cursor_row, cursor_col) = self.screen.cursor();
            let rows: Vec<_> = lines
                .into_iter()
                .enumerate()
                .map(|(row, line)| {
                    let mut runs: Vec<TextRun> = line
                        .spans
                        .iter()
                        .map(|s| TextRun {
                            len: s.len,
                            font: Font {
                                weight: if s.bold {
                                    gpui::FontWeight::BOLD
                                } else {
                                    Default::default()
                                },
                                style: if s.italic {
                                    gpui::FontStyle::Italic
                                } else {
                                    Default::default()
                                },
                                ..self.font.clone()
                            },
                            color: hsla(s.fg),
                            background_color: Some(hsla(s.bg)),
                            underline: s.underline.then(|| UnderlineStyle {
                                thickness: px(1.),
                                ..Default::default()
                            }),
                            strikethrough: None,
                        })
                        .collect();
                    // The cursor: its cell drawn inverted.
                    if row == cursor_row {
                        let at = line.text.char_indices().nth(cursor_col).map(|(i, _)| i);
                        if let Some(at) = at {
                            let len = line.text[at..].chars().next().map_or(1, char::len_utf8);
                            runs = cursor_runs(runs, at, len);
                        }
                    }
                    StyledText::new(line.text).with_runs(runs)
                })
                .collect();
            self.spent += start.elapsed();
            self.frames += 1;
            if self.since.elapsed() > Duration::from_secs(2) {
                eprintln!(
                    "{} frames, lines built in {:?} each, window {:?}",
                    self.frames,
                    self.spent / self.frames.max(1),
                    window.bounds().size
                );
                (self.frames, self.spent, self.since) = (0, Duration::ZERO, Instant::now());
            }
            div()
                .track_focus(&self.focus)
                .on_key_down(_cx.listener(|this, e: &KeyDownEvent, _, _| {
                    if let Some(b) = bytes(e) {
                        let _ = this.pty.write_all(&b);
                    }
                }))
                .size_full()
                .bg(gpui::rgb(0x1e1e1e))
                .p_2()
                .font(self.font.clone())
                .text_size(px(13.))
                .line_height(px(17.))
                .children(rows)
        }
    }

    /// Splits the runs at the cursor's cell and swaps that cell's colours.
    fn cursor_runs(runs: Vec<TextRun>, at: usize, len: usize) -> Vec<TextRun> {
        let mut out = Vec::new();
        let mut pos = 0;
        for r in runs {
            let (start, end) = (pos, pos + r.len);
            pos = end;
            if end <= at || start >= at + len {
                out.push(r);
                continue;
            }
            if at > start {
                out.push(TextRun {
                    len: at - start,
                    ..r.clone()
                });
            }
            out.push(TextRun {
                len,
                color: r.background_color.unwrap_or(gpui::black()),
                background_color: Some(r.color),
                ..r.clone()
            });
            if end > at + len {
                out.push(TextRun {
                    len: end - at - len,
                    ..r
                });
            }
        }
        out
    }

    pub fn main() {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let size_ = WindowSize {
            num_lines: LINES,
            num_cols: COLUMNS,
            cell_width: 8,
            cell_height: 17,
        };
        let options = Options {
            shell: Some(Shell::new(shell, vec!["-l".into()])),
            env: [("TERM".to_string(), "xterm-256color".to_string())].into(),
            ..Default::default()
        };
        let pty = tty::new(&options, size_, 0).expect("starting the shell");
        let file = pty.file().try_clone().expect("the PTY");
        // The PTY is non-blocking, for alacritty's own event loop; this reads it
        // on a thread of its own.
        #[cfg(unix)]
        unsafe {
            use std::os::fd::AsRawFd;
            let fd = file.as_raw_fd();
            libc::fcntl(
                fd,
                libc::F_SETFL,
                libc::fcntl(fd, libc::F_GETFL) & !libc::O_NONBLOCK,
            );
        }
        if let Some(cmd) = std::env::args().nth(1) {
            let _ = (&file).write_all(format!("{cmd}\r").as_bytes());
        }
        // Bounded, so a program writing faster than the screen takes it waits,
        // as it would on any terminal.
        let (tx, rx) = async_channel::bounded::<Vec<u8>>(8);
        let mut reader = file.try_clone().expect("the PTY");
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 64 << 10];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send_blocking(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });

        application().run(move |cx: &mut App| {
            let bounds = Bounds::centered(None, size(px(1000.), px(640.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |window, cx| {
                    cx.new(|cx| {
                        let focus = cx.focus_handle();
                        window.focus(&focus, cx);
                        // Output arrives in bursts: take what has come, for at
                        // most a few milliseconds, then let a frame be drawn.
                        cx.spawn(async move |this, cx| {
                            while let Ok(first) = rx.recv().await {
                                let _ = this.update(cx, |t: &mut Terminal, cx| {
                                    let start = Instant::now();
                                    t.screen.feed(&first);
                                    while start.elapsed() < Duration::from_millis(4) {
                                        let Ok(more) = rx.try_recv() else { break };
                                        t.screen.feed(&more);
                                    }
                                    cx.notify();
                                });
                            }
                        })
                        .detach();
                        Terminal {
                            screen: Screen::new(COLUMNS as usize, LINES as usize),
                            pty: file,
                            focus,
                            font: font(if cfg!(target_os = "macos") {
                                "Menlo"
                            } else {
                                "DejaVu Sans Mono"
                            }),
                            frames: 0,
                            spent: Duration::ZERO,
                            since: Instant::now(),
                        }
                    })
                },
            )
            .unwrap();
            cx.activate(true);
        });
        drop(pty);
    }
}

fn main() {
    #[cfg(unix)]
    unix::main();
}
