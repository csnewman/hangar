//! How big the window's panels are: the sidebar, and each environment tab's
//! tool windows on the left, the right and along the bottom. Dragged by
//! their edges, reset by double-clicking one, and kept between runs.

use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    App, AppContext, Axis, Bounds, Global, IntoElement, Pixels, Render, Task, Window, div, px,
};
use gpui_component::InteractiveElementExt;
use gpui_component::resizable::{HandleEdge, resize_handle};
use serde::{Deserialize, Serialize};

/// A panel edge that can be dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Sidebar,
    Left,
    Right,
    Bottom,
}

impl Edge {
    fn default_size(self) -> f32 {
        match self {
            Edge::Sidebar => 240.,
            Edge::Left => 220.,
            Edge::Right => 260.,
            Edge::Bottom => 190.,
        }
    }

    fn range(self) -> (f32, f32) {
        match self {
            Edge::Sidebar => (180., 440.),
            Edge::Left | Edge::Right => (150., 640.),
            Edge::Bottom => (80., 900.),
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
}

impl Default for Sizes {
    fn default() -> Self {
        Sizes {
            sidebar: Edge::Sidebar.default_size(),
            left: Edge::Left.default_size(),
            right: Edge::Right.default_size(),
            bottom: Edge::Bottom.default_size(),
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
        })
    }

    /// Sets an edge's panel to size, within what it may be.
    pub fn set(&mut self, e: Edge, size: f32) {
        let (lo, hi) = e.range();
        let size = size.clamp(lo, hi).round();
        match e {
            Edge::Sidebar => self.sidebar = size,
            Edge::Left => self.left = size,
            Edge::Right => self.right = size,
            Edge::Bottom => self.bottom = size,
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
        }
    }
}

/// The sizes, app-wide, and where they are kept.
pub struct Layout {
    pub sizes: Sizes,
    file: Option<PathBuf>,
    saving: Option<Task<()>>,
}

impl Global for Layout {}

/// How long a drag's last change waits before the sizes are written.
const SAVE_AFTER: Duration = Duration::from_millis(500);

impl Layout {
    /// The sizes kept in file, or the defaults.
    pub fn init(file: Option<PathBuf>, cx: &mut App) {
        let sizes = file
            .as_ref()
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        cx.set_global(Layout {
            sizes,
            file,
            saving: None,
        });
    }

    pub fn sizes(cx: &App) -> Sizes {
        cx.try_global::<Layout>()
            .map(|l| l.sizes)
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
        if layout.sizes == before {
            return;
        }
        if layout.file.is_some() && layout.saving.is_none() {
            let task = cx.spawn(async move |cx| {
                cx.background_executor().timer(SAVE_AFTER).await;
                cx.update(|cx| {
                    let l = cx.global_mut::<Layout>();
                    l.saving = None;
                    if let (Some(file), Ok(b)) = (&l.file, serde_json::to_vec_pretty(&l.sizes)) {
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
    };
    resize_handle::<Edge, Ghost>(id, axis)
        .inside(side)
        .on_drag(edge, |_, _, _, cx| cx.new(|_| Ghost))
        .on_double_click(move |_, _, cx| Layout::update(cx, |s| s.reset(edge)))
}
