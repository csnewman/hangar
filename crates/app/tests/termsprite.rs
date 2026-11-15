//! Box drawing and blocks fill their cells, so lines and bars meet the
//! next row's and the next column's.

use hangar_app::termsprite::{Shape, shapes};

const W: f32 = 8.;
const H: f32 = 18.;
const T: f32 = 1.;

/// The bounds the rectangles of a character's shapes cover: left, top,
/// right, bottom.
fn extent(c: char) -> (f32, f32, f32, f32) {
    let mut e = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for s in shapes(c, W, H, T).expect("a shape") {
        if let Shape::Rect { x, y, w, h, .. } = s {
            e = (e.0.min(x), e.1.min(y), e.2.max(x + w), e.3.max(y + h));
        }
    }
    e
}

#[test]
fn lines_reach_the_edges_they_join() {
    let (l, top, r, b) = extent('─');
    assert_eq!((l, r), (0., W), "across the cell");
    assert_eq!(
        (top, b),
        (H / 2. - T / 2., H / 2. + T / 2.),
        "through its middle"
    );
    let (l, top, r, b) = extent('│');
    assert_eq!((top, b), (0., H), "down the cell");
    assert_eq!((l, r), (W / 2. - T / 2., W / 2. + T / 2.));
    assert_eq!(extent('┼'), (0., 0., W, H));
    // A corner reaches the two edges it joins, and no others.
    let (l, top, r, b) = extent('┌');
    assert_eq!((r, b), (W, H));
    assert!(l > 0. && top > 0.);
    // Heavy lines are twice as thick.
    let (_, top, _, b) = extent('━');
    assert_eq!(b - top, 2. * T);
}

#[test]
fn double_lines_are_two_rails() {
    let rails = shapes('═', W, H, T).unwrap();
    let mut ys: Vec<f32> = rails
        .iter()
        .map(|s| match s {
            Shape::Rect { y, .. } => *y,
            _ => panic!("{s:?}"),
        })
        .collect();
    ys.sort_by(f32::total_cmp);
    ys.dedup();
    assert_eq!(ys, [H / 2. - 1.5 * T, H / 2. + 0.5 * T]);
}

#[test]
fn blocks_fill_their_part_of_the_cell() {
    assert_eq!(extent('█'), (0., 0., W, H));
    assert_eq!(extent('▀'), (0., 0., W, H / 2.));
    assert_eq!(extent('▄'), (0., H / 2., W, H));
    assert_eq!(extent('▐'), (W / 2., 0., W, H));
    match &shapes('░', W, H, T).unwrap()[0] {
        Shape::Rect { alpha, .. } => assert_eq!(*alpha, 0.25),
        s => panic!("{s:?}"),
    }
}

#[test]
fn arcs_join_the_middles_of_their_edges() {
    match &shapes('╭', W, H, T).unwrap()[0] {
        Shape::Stroke { points, .. } => {
            assert_eq!(points.first(), Some(&(W, H / 2.)), "from the right");
            assert_eq!(points.last(), Some(&(W / 2., H)), "to the bottom");
        }
        s => panic!("{s:?}"),
    }
}

#[test]
fn other_characters_are_the_fonts() {
    assert_eq!(shapes('a', W, H, T), None);
    assert_eq!(shapes('⏺', W, H, T), None);
}
