//! A terminal's rectangles are joined where they meet, so a screen of box
//! drawing and backgrounds costs a few to paint, not one a cell.

use gpui::{Bounds, Hsla, point, px, size};
use hangar_app::termpaint::{Rect, coalesce};

fn rect(x: f32, y: f32, w: f32, h: f32, c: Hsla) -> Rect {
    Rect {
        b: Bounds::new(point(px(x), px(y)), size(px(w), px(h))),
        c,
    }
}

#[test]
fn rectangles_that_meet_are_joined() {
    let (red, blue) = (gpui::red(), gpui::blue());
    // A rule of four cells, each two halves, as box drawing's ─ is.
    let mut rects = vec![];
    for cell in 0..4 {
        let x = cell as f32 * 8.;
        rects.push(rect(x, 8., 4., 1., red));
        rects.push(rect(x + 4., 8., 4., 1., red));
    }
    assert_eq!(coalesce(rects), [rect(0., 8., 32., 1., red)]);
    // A column of three rows, and a blue cell beside one: apart.
    let rects = vec![
        rect(3.5, 0., 1., 18., red),
        rect(3.5, 18., 1., 18., red),
        rect(3.5, 36., 1., 18., red),
        rect(8., 18., 8., 18., blue),
    ];
    let mut out = coalesce(rects);
    out.sort_by(|a, b| f32::from(a.b.origin.x).total_cmp(&f32::from(b.b.origin.x)));
    assert_eq!(
        out,
        [rect(3.5, 0., 1., 54., red), rect(8., 18., 8., 18., blue)]
    );
    // Side by side but of two colours: apart.
    let two = vec![rect(0., 0., 8., 18., red), rect(8., 0., 8., 18., blue)];
    assert_eq!(coalesce(two).len(), 2);
}
