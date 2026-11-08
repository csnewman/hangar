//! The terminal in a window: Tab is the shell's, not the window's way to
//! move between fields.

use gpui::{
    AppContext, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    TestAppContext, VisualTestContext, Window, div,
};
use gpui_component::Root;
use gpui_component::input::{Textarea, TextareaState};
use hangar_app::api::Client;
use hangar_app::termview::TerminalView;

/// A terminal with a text field after it, for Tab to move focus to.
struct Both {
    term: Entity<TerminalView>,
    field: Entity<TextareaState>,
}

impl Render for Both {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .child(self.term.clone())
            .child(Textarea::new(&self.field))
    }
}

fn open(cx: &mut TestAppContext) -> (FocusHandle, FocusHandle, &mut VisualTestContext) {
    cx.update(gpui_component::init);
    cx.update(hangar_app::termview::bind_keys);
    // Nothing answers there: the terminal fails to connect, and still
    // takes keys.
    let client = Client::new("http://127.0.0.1:9", "hgr_t");
    let (root, cx) = cx.add_window_view(|window, cx| {
        let term = cx.new(|cx| TerminalView::new(&client, "env1", None, cx));
        let field = cx.new(|cx| TextareaState::new(window, cx));
        let both = cx.new(|_| Both { term, field });
        Root::new(both, window, cx)
    });
    let both: Entity<Both> = root.read_with(cx, |r, _| r.view().clone().downcast().unwrap());
    let (term, field) = both.read_with(cx, |b, cx| {
        (b.term.focus_handle(cx), b.field.focus_handle(cx))
    });
    cx.update(|window, cx| window.focus(&term, cx));
    cx.run_until_parked();
    (term, field, cx)
}

#[gpui::test]
fn tab_stays_in_the_terminal(cx: &mut TestAppContext) {
    let (term, field, cx) = open(cx);
    cx.simulate_keystrokes("tab");
    cx.update(|window, cx| {
        assert!(
            term.is_focused(window),
            "Tab moved focus out of the terminal"
        );
        assert!(!field.contains_focused(window, cx));
    });
    cx.simulate_keystrokes("shift-tab");
    cx.update(|window, _| assert!(term.is_focused(window), "Shift-Tab moved focus"));
}
