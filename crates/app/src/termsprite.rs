//! Box drawing, block elements and powerline's arrows as shapes that fill
//! their cells, as Ghostty draws them. A font's glyphs for them stop short
//! of a cell's edges where the line is taller than the font, so boxes and
//! bars drawn from them have gaps between the rows.

/// A shape in a cell, in points from its top left.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    /// A filled rectangle, in a share of the colour: shades are less.
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        alpha: f32,
    },
    /// A line through the points, of a width.
    Stroke { points: Vec<(f32, f32)>, width: f32 },
    /// A filled polygon.
    Fill { points: Vec<(f32, f32)> },
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Shape {
    Shape::Rect {
        x,
        y,
        w,
        h,
        alpha: 1.,
    }
}

/// The lines of the box drawing characters U+2500 to U+257F, each as the
/// weight of its arm up, right, down and left: 0 none, 1 light, 2 heavy,
/// 3 double. Empty for the dashed lines, the arcs and the diagonals, drawn
/// apart.
const ARMS: [&str; 128] = [
    "0101", "0202", "1010", "2020", "", "", "", "", "", "", "", "", // 2500
    "0110", "0210", "0120", "0220", // 250C
    "0011", "0012", "0021", "0022", "1100", "1200", "2100", "2200", // 2510
    "1001", "1002", "2001", "2002", "1110", "1210", "2110", "1120", // 2518
    "2120", "2210", "1220", "2220", "1011", "1012", "2011", "1021", // 2520
    "2021", "2012", "1022", "2022", "0111", "0112", "0211", "0212", // 2528
    "0121", "0122", "0221", "0222", "1101", "1102", "1201", "1202", // 2530
    "2101", "2102", "2201", "2202", "1111", "1112", "1211", "1212", // 2538
    "2111", "1121", "2121", "2112", "2211", "1122", "1221", "2212", // 2540
    "1222", "2122", "2221", "2222", "", "", "", "", // 2548
    "0303", "3030", "0310", "0130", "0330", "0013", "0031", "0033", // 2550
    "1300", "3100", "3300", "1003", "3001", "3003", "1310", "3130", // 2558
    "3330", "1013", "3031", "3033", "0313", "0131", "0333", "1303", // 2560
    "3101", "3303", "1313", "3131", "3333", "", "", "", // 2568
    "", "", "", "", "0001", "1000", "0100", "0010", // 2570
    "0002", "2000", "0200", "0020", "0201", "1020", "0102", "2010", // 2578
];

/// The shapes a character is drawn with in a cell w by h whose light lines
/// are `light` thick; None for a character a font draws.
pub fn shapes(c: char, w: f32, h: f32, light: f32) -> Option<Vec<Shape>> {
    let code = c as u32;
    match code {
        0x2500..=0x257f => lines(code, w, h, light),
        0x2580..=0x259f => Some(blocks(code, w, h)),
        0xe0b0..=0xe0b3 => Some(arrows(code, w, h, light)),
        _ => None,
    }
}

/// Box drawing: lines from the cell's middle to its edges.
fn lines(code: u32, w: f32, h: f32, t: f32) -> Option<Vec<Shape>> {
    let arms = ARMS[(code - 0x2500) as usize];
    if arms.is_empty() {
        return special(code, w, h, t);
    }
    let a: Vec<u8> = arms.bytes().map(|b| b - b'0').collect();
    // Up, right, down, left.
    let [up, right, down, left] = [a[0], a[1], a[2], a[3]];
    let (cx, cy) = (w / 2., h / 2.);
    // How far a line of a weight reaches either side of the middle.
    let half = |weight: u8| match weight {
        0 => 0.,
        1 => t / 2.,
        2 => t,
        _ => 1.5 * t,
    };
    let thick = |weight: u8| if weight == 2 { 2. * t } else { t };
    let mut out = Vec::new();
    // Each arm, from its edge towards the middle, with the arms either
    // side of it (before its middle line, then after) and the one opposite.
    struct Arm {
        weight: u8,
        before: u8,
        after: u8,
        opposite: u8,
    }
    let horizontal = [
        (
            Arm {
                weight: left,
                before: up,
                after: down,
                opposite: right,
            },
            -1.,
        ),
        (
            Arm {
                weight: right,
                before: up,
                after: down,
                opposite: left,
            },
            1.,
        ),
    ];
    let vertical = [
        (
            Arm {
                weight: up,
                before: left,
                after: right,
                opposite: down,
            },
            -1.,
        ),
        (
            Arm {
                weight: down,
                before: left,
                after: right,
                opposite: up,
            },
            1.,
        ),
    ];
    // Where a line of an arm stops, short of the middle (negative) or past
    // it, given the arms beside it on its side and the far side.
    let reach = |near: u8, far: u8, opposite: u8| -> f32 {
        if near == 3 {
            -0.5 * t
        } else if near > 0 {
            -half(near)
        } else if opposite > 0 {
            0.
        } else {
            half(far)
        }
    };
    for (horiz, arms) in [(true, &horizontal), (false, &vertical)] {
        for (arm, dir) in arms.iter() {
            if arm.weight == 0 {
                continue;
            }
            let (mid, size) = if horiz { (cx, w) } else { (cy, h) };
            let other_mid = if horiz { cy } else { cx };
            // The line's span along the arm: from the edge to `to`, which
            // is past the middle by `beyond`.
            let span = |beyond: f32| -> (f32, f32) {
                if *dir < 0. {
                    (0., mid + beyond)
                } else {
                    (mid - beyond, size)
                }
            };
            let mut put = |beyond: f32, centre: f32, thickness: f32| {
                let (from, to) = span(beyond);
                let (from, to) = (from.max(0.), to.min(size));
                if to <= from {
                    return;
                }
                let across = centre - thickness / 2.;
                out.push(if horiz {
                    rect(from, across, to - from, thickness)
                } else {
                    rect(across, from, thickness, to - from)
                });
            };
            if arm.weight == 3 {
                // Two rails, a light line's width apart: the one before the
                // middle line, then the one after.
                put(reach(arm.before, arm.after, arm.opposite), other_mid - t, t);
                put(reach(arm.after, arm.before, arm.opposite), other_mid + t, t);
            } else {
                // Across whatever crosses it, to meet the widest; else to the
                // middle, where the arm opposite meets it.
                put(
                    half(arm.before).max(half(arm.after)),
                    other_mid,
                    thick(arm.weight),
                );
            }
        }
    }
    Some(out)
}

/// The dashed lines, the arcs and the diagonals.
fn special(code: u32, w: f32, h: f32, t: f32) -> Option<Vec<Shape>> {
    let (cx, cy) = (w / 2., h / 2.);
    // A dashed line: a dash in each of n equal parts, centred in it, so
    // cells side by side keep the rhythm.
    let dashes = |n: usize, horiz: bool, thickness: f32| -> Vec<Shape> {
        let length = if horiz { w } else { h };
        let part = length / n as f32;
        let dash = part * 0.6;
        (0..n)
            .map(|i| {
                let from = i as f32 * part + (part - dash) / 2.;
                if horiz {
                    rect(from, cy - thickness / 2., dash, thickness)
                } else {
                    rect(cx - thickness / 2., from, thickness, dash)
                }
            })
            .collect()
    };
    let heavy = 2. * t;
    Some(match code {
        0x2504 => dashes(3, true, t),
        0x2505 => dashes(3, true, heavy),
        0x2506 => dashes(3, false, t),
        0x2507 => dashes(3, false, heavy),
        0x2508 => dashes(4, true, t),
        0x2509 => dashes(4, true, heavy),
        0x250a => dashes(4, false, t),
        0x250b => dashes(4, false, heavy),
        0x254c => dashes(2, true, t),
        0x254d => dashes(2, true, heavy),
        0x254e => dashes(2, false, t),
        0x254f => dashes(2, false, heavy),
        // The arcs: a quarter circle joining two arms, straight beyond it.
        0x256d..=0x2570 => {
            let r = cx.min(cy);
            // The arms each arc joins: to the right or left, and down or up.
            let (sx, sy) = match code {
                0x256d => (1., 1.),
                0x256e => (-1., 1.),
                0x256f => (-1., -1.),
                _ => (1., -1.),
            };
            let centre = (cx + sx * r, cy + sy * r);
            let mut points = vec![(if sx > 0. { w } else { 0. }, cy)];
            let steps = 12;
            for i in 0..=steps {
                // From the horizontal arm's end round to the vertical's.
                let a = std::f32::consts::FRAC_PI_2 * i as f32 / steps as f32;
                points.push((centre.0 - sx * r * a.sin(), centre.1 - sy * r * a.cos()));
            }
            points.push((cx, if sy > 0. { h } else { 0. }));
            vec![Shape::Stroke { points, width: t }]
        }
        0x2571 => vec![Shape::Stroke {
            points: vec![(w, 0.), (0., h)],
            width: t,
        }],
        0x2572 => vec![Shape::Stroke {
            points: vec![(0., 0.), (w, h)],
            width: t,
        }],
        0x2573 => vec![
            Shape::Stroke {
                points: vec![(w, 0.), (0., h)],
                width: t,
            },
            Shape::Stroke {
                points: vec![(0., 0.), (w, h)],
                width: t,
            },
        ],
        _ => return None,
    })
}

/// Block elements: parts of the cell filled, and shades.
fn blocks(code: u32, w: f32, h: f32) -> Vec<Shape> {
    let eighth = |n: u32| n as f32 / 8.;
    let (hw, hh) = (w / 2., h / 2.);
    let quadrants =
        |q: &[(f32, f32)]| -> Vec<Shape> { q.iter().map(|&(x, y)| rect(x, y, hw, hh)).collect() };
    let (ul, ur, ll, lr) = ((0., 0.), (hw, 0.), (0., hh), (hw, hh));
    let shade = |alpha: f32| {
        vec![Shape::Rect {
            x: 0.,
            y: 0.,
            w,
            h,
            alpha,
        }]
    };
    match code {
        0x2580 => vec![rect(0., 0., w, hh)],
        0x2581..=0x2588 => {
            let part = h * eighth(code - 0x2580);
            vec![rect(0., h - part, w, part)]
        }
        0x2589..=0x258f => vec![rect(0., 0., w * eighth(0x2590 - code), h)],
        0x2590 => vec![rect(hw, 0., hw, h)],
        0x2591 => shade(0.25),
        0x2592 => shade(0.5),
        0x2593 => shade(0.75),
        0x2594 => vec![rect(0., 0., w, h * eighth(1))],
        0x2595 => vec![rect(w * eighth(7), 0., w * eighth(1), h)],
        0x2596 => quadrants(&[ll]),
        0x2597 => quadrants(&[lr]),
        0x2598 => quadrants(&[ul]),
        0x2599 => quadrants(&[ul, ll, lr]),
        0x259a => quadrants(&[ul, lr]),
        0x259b => quadrants(&[ul, ur, ll]),
        0x259c => quadrants(&[ul, ur, lr]),
        0x259d => quadrants(&[ur]),
        0x259e => quadrants(&[ur, ll]),
        _ => quadrants(&[ur, ll, lr]),
    }
}

/// Powerline's arrows, filling the cell's height.
fn arrows(code: u32, w: f32, h: f32, t: f32) -> Vec<Shape> {
    let right = vec![(0., 0.), (w, h / 2.), (0., h)];
    let left = vec![(w, 0.), (0., h / 2.), (w, h)];
    match code {
        0xe0b0 => vec![Shape::Fill { points: right }],
        0xe0b1 => vec![Shape::Stroke {
            points: right,
            width: t,
        }],
        0xe0b2 => vec![Shape::Fill { points: left }],
        _ => vec![Shape::Stroke {
            points: left,
            width: t,
        }],
    }
}
