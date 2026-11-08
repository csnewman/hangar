//! How big the window's panels are: the sidebar, and each environment tab's
//! tool windows on the left, the right and along the bottom. Dragged by
//! their edges, reset by double-clicking one, and kept between runs, with
//! where each tool window docks.

use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    App, AppContext, Axis, Bounds, Global, IntoElement, Pixels, Render, Task, Window, div, px,
};
use gpui_component::InteractiveElementExt;
use gpui_component::resizable::{HandleEdge, resize_handle};
use serde::{Deserialize, Serialize};

use crate::tools::{Slot, Stripes, Tool};

/// A panel edge that can be dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Sidebar,
    Left,
    Right,
    Bottom,
    /// Between a side's tool window and the one below the line, as a
    /// fraction of the side's height.
    LeftSplit,
    RightSplit,
}

impl Edge {
    fn default_size(self) -> f32 {
        match self {
            Edge::Sidebar => 240.,
            Edge::Left => 220.,
            Edge::Right => 260.,
            Edge::Bottom => 190.,
            Edge::LeftSplit | Edge::RightSplit => 0.5,
        }
    }

    fn range(self) -> (f32, f32) {
        match self {
            Edge::Sidebar => (180., 440.),
            Edge::Left | Edge::Right => (150., 640.),
            Edge::Bottom => (80., 900.),
            Edge::LeftSplit | Edge::RightSplit => (0.15, 0.85),
        }
    }
}

/// The panels' sizes, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sizes {
    pub sidebar: f32,
    pub left: f32,
    pub right: f32,
    pub bottom: f32,
    pub left_split: f32,
    pub right_split: f32,
}

impl Default for Sizes {
    fn default() -> Self {
        Sizes {
            sidebar: Edge::Sidebar.default_size(),
            left: Edge::Left.default_size(),
            right: Edge::Right.default_size(),
            bottom: Edge::Bottom.default_size(),
            left_split: Edge::LeftSplit.default_size(),
            right_split: Edge::RightSplit.default_size(),
        }
    }
}

impl Sizes {
    pub fn get(&self, e: Edge) -> Pixels {
        px(match e {
            Edge::Sidebar => self.sidebar,
            Edge::Left => self.left,
            Edge::Right => self.right,
            Edge::Bottom => self.bottom,
            Edge::LeftSplit => self.left_split,
            Edge::RightSplit => self.right_split,
        })
    }

    /// A split's fraction: how much of its side the upper window takes.
    pub fn fraction(&self, e: Edge) -> f32 {
        match e {
            Edge::LeftSplit => self.left_split,
            Edge::RightSplit => self.right_split,
            _ => 1.,
        }
    }

    /// Sets an edge's panel to size, within what it may be.
    pub fn set(&mut self, e: Edge, size: f32) {
        let (lo, hi) = e.range();
        let size = size.clamp(lo, hi);
        match e {
            Edge::Sidebar => self.sidebar = size.round(),
            Edge::Left => self.left = size.round(),
            Edge::Right => self.right = size.round(),
            Edge::Bottom => self.bottom = size.round(),
            Edge::LeftSplit => self.left_split = (size * 1000.).round() / 1000.,
            Edge::RightSplit => self.right_split = (size * 1000.).round() / 1000.,
        }
    }

    pub fn reset(&mut self, e: Edge) {
        self.set(e, e.default_size());
    }

    /// The size an edge's panel takes when the edge is dragged to position,
    /// in the area whose bounds are given, where inset is what lies between
    /// the area's side and the panel: a stripe, or the status bar.
    pub fn dragged(
        e: Edge,
        position: gpui::Point<Pixels>,
        area: Bounds<Pixels>,
        inset: f32,
    ) -> f32 {
        let f = f32::from;
        match e {
            Edge::Sidebar | Edge::Left => f(position.x) - f(area.left()) - inset,
            Edge::Right => f(area.right()) - f(position.x) - inset,
            Edge::Bottom => f(area.bottom()) - f(position.y) - inset,
            Edge::LeftSplit | Edge::RightSplit => {
                (f(position.y) - f(area.top())) / f(area.size.height).max(1.)
            }
        }
    }
}

/// The sizes and the stripes' arrangement, app-wide, and where they are
/// kept.
pub struct Layout {
    pub sizes: Sizes,
    pub stripes: Stripes,
    file: Option<PathBuf>,
    saving: Option<Task<()>>,
}

/// The layout as kept: the sizes, then the stripes.
#[derive(Default, Serialize, Deserialize)]
struct Kept {
    #[serde(flatten)]
    sizes: Sizes,
    #[serde(default)]
    stripes: Stripes,
}

impl Global for Layout {}

/// How long a drag's last change waits before the sizes are written.
const SAVE_AFTER: Duration = Duration::from_millis(500);

impl Layout {
    /// The layout kept in file, or the defaults.
    pub fn init(file: Option<PathBuf>, cx: &mut App) {
        let kept: Kept = file
            .as_ref()
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        cx.set_global(Layout {
            sizes: kept.sizes,
            stripes: kept.stripes,
            file,
            saving: None,
        });
    }

    pub fn sizes(cx: &App) -> Sizes {
        cx.try_global::<Layout>()
            .map(|l| l.sizes)
            .unwrap_or_default()
    }

    /// Where a tool window opens.
    pub fn slot(t: Tool, cx: &App) -> Slot {
        match cx.try_global::<Layout>() {
            Some(l) => l.stripes.slot(t),
            None => Stripes::default().slot(t),
        }
    }

    pub fn stripes(cx: &App) -> Stripes {
        cx.try_global::<Layout>()
            .map(|l| l.stripes.clone())
            .unwrap_or_default()
    }

    /// Changes the sizes, and keeps them a moment later.
    pub fn update(cx: &mut App, f: impl FnOnce(&mut Sizes)) {
        if !cx.has_global::<Layout>() {
            Layout::init(None, cx);
        }
        let before = Layout::sizes(cx);
        let layout = cx.global_mut::<Layout>();
        f(&mut layout.sizes);
        if layout.sizes != before {
            Layout::changed(cx);
        }
    }

    /// Changes the stripes' arrangement, and keeps it a moment later.
    pub fn arrange(cx: &mut App, f: impl FnOnce(&mut Stripes)) {
        if !cx.has_global::<Layout>() {
            Layout::init(None, cx);
        }
        let before = Layout::stripes(cx);
        let layout = cx.global_mut::<Layout>();
        f(&mut layout.stripes);
        if layout.stripes != before {
            Layout::changed(cx);
        }
    }

    fn changed(cx: &mut App) {
        let layout = cx.global_mut::<Layout>();
        if layout.file.is_some() && layout.saving.is_none() {
            let task = cx.spawn(async move |cx| {
                cx.background_executor().timer(SAVE_AFTER).await;
                cx.update(|cx| {
                    let l = cx.global_mut::<Layout>();
                    l.saving = None;
                    let kept = Kept {
                        sizes: l.sizes,
                        stripes: l.stripes.clone(),
                    };
                    if let (Some(file), Ok(b)) = (&l.file, serde_json::to_vec_pretty(&kept)) {
                        if let Some(dir) = file.parent() {
                            let _ = std::fs::create_dir_all(dir);
                        }
                        let _ = std::fs::write(file, b);
                    }
                });
            });
            cx.global_mut::<Layout>().saving = Some(task);
        }
        cx.refresh_windows();
    }
}

/// What a drag of an edge shows under the pointer: nothing, the panel
/// itself moving.
pub struct Ghost;

impl Render for Ghost {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
    }
}

/// The handle on a panel's edge: dragged, it carries the edge for the area
/// around it to follow with on_drag_move; double-clicked, it puts the panel
/// back to its usual size.
pub fn handle(id: &'static str, edge: Edge) -> impl IntoElement {
    let (axis, side) = match edge {
        Edge::Sidebar | Edge::Left => (Axis::Horizontal, HandleEdge::Trailing),
        Edge::Right => (Axis::Horizontal, HandleEdge::Leading),
        Edge::Bottom => (Axis::Vertical, HandleEdge::Leading),
        Edge::LeftSplit | Edge::RightSplit => (Axis::Vertical, HandleEdge::Trailing),
    };
    resize_handle::<Edge, Ghost>(id, axis)
        .inside(side)
        .on_drag(edge, |_, _, _, cx| cx.new(|_| Ghost))
        .on_double_click(move |_, _, cx| Layout::update(cx, |s| s.reset(edge)))
}
