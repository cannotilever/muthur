//! The container list: an ID-following selection and the list screen.

use crate::metrics::Container;
use crate::utility::widgets::panel;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;

/// The STATUS column: fixed, so a status changing width (an in-flight
/// `RESTARTING`) never shifts every row. Fits every runtime state word.
pub(super) const STATUS_W: usize = 10;
/// Inner rows that are not containers: heading, message line, hint line.
const CHROME_ROWS: u16 = 3;

/// The selected container (by ID, so a re-sort does not move the cursor to a
/// different one) and the viewport that keeps it visible.
pub(super) struct Selection {
    id: Option<String>,
    index: usize,
    /// First container row shown.
    scroll: usize,
    /// Container rows the last render had room for; `None` before the first
    /// render, when the viewport is not known and must not be moved.
    page: Option<usize>,
}

impl Selection {
    pub(super) fn new() -> Self {
        Self {
            id: None,
            index: 0,
            scroll: 0,
            page: None,
        }
    }

    /// After the list changed: find the selected ID again, or keep the row
    /// position (clamped) when that container is gone.
    pub(super) fn sync(&mut self, list: &[Container]) {
        if let Some(i) = self.id.as_ref().and_then(|id| list.iter().position(|c| &c.id == id)) {
            self.index = i;
        }
        self.settle(list);
    }

    pub(super) fn up(&mut self, list: &[Container]) {
        self.index = self.index.saturating_sub(1);
        self.settle(list);
    }

    pub(super) fn down(&mut self, list: &[Container]) {
        self.index += 1;
        self.settle(list);
    }

    fn page(&self) -> usize {
        self.page.unwrap_or(1)
    }

    pub(super) fn page_up(&mut self, list: &[Container]) {
        self.index = self.index.saturating_sub(self.page());
        self.scroll = self.scroll.saturating_sub(self.page());
        self.settle(list);
    }

    pub(super) fn page_down(&mut self, list: &[Container]) {
        self.index += self.page();
        self.scroll += self.page();
        self.settle(list);
    }

    pub(super) fn home(&mut self, list: &[Container]) {
        self.index = 0;
        self.scroll = 0;
        self.settle(list);
    }

    pub(super) fn selected<'a>(&self, list: &'a [Container]) -> Option<&'a Container> {
        list.get(self.index)
    }

    /// Clamp index and scroll to the list, keep the selection in view, and
    /// remember which container it is.
    fn settle(&mut self, list: &[Container]) {
        self.index = self.index.min(list.len().saturating_sub(1));
        self.id = list.get(self.index).map(|c| c.id.clone());
        let Some(page) = self.page else { return };
        self.scroll = self.scroll.min(list.len().saturating_sub(page));
        if self.index < self.scroll {
            self.scroll = self.index;
        } else if self.index >= self.scroll + page {
            self.scroll = self.index + 1 - page;
        }
    }
}

/// The list screen. `status_override` replaces one row's STATUS cell
/// (`(id, "RESTARTING...")`); `message` is the detail/message line and
/// whether it is drawn in reverse video.
pub(super) fn draw_list(
    sel: &mut Selection,
    list: &[Container],
    status_override: Option<(&str, &str)>,
    message: (&str, bool),
    hint: &str,
    area: Rect,
    buf: &mut Buffer,
) {
    let page = area.height.saturating_sub(2 + CHROME_ROWS).max(1) as usize;
    sel.page = Some(page);
    sel.settle(list);
    let shown = &list[sel.scroll..list.len().min(sel.scroll + page)];
    let noun = if list.len() == 1 { "CONTAINER" } else { "CONTAINERS" };
    let mut title = format!("CARGO - {} {noun}", list.len());
    if shown.len() < list.len() {
        title += &format!(" ({}-{})", sel.scroll + 1, sel.scroll + shown.len());
    }
    let block = panel(&title);
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height < CHROME_ROWS + 1 {
        return;
    }

    let w = inner.width as usize;
    let status_of = |k: &Container| match status_override {
        Some((id, s)) if id == k.id => s.to_string(),
        _ => k.status.clone(),
    };
    let status_w = STATUS_W.min(w / 2);
    let name_w = w.saturating_sub(1 + status_w + 1);
    let cut = |s: &str, n: usize| s.chars().take(n).collect::<String>();
    let status_x = inner.x + (1 + name_w + 1) as u16;
    buf.set_string(inner.x, inner.y, format!(" {:<name_w$} {}", "NAME", "STATUS"), Style::new());
    for (i, k) in shown.iter().enumerate() {
        let y = inner.y + 1 + i as u16;
        let selected = sel.scroll + i == sel.index;
        buf.set_string(inner.x, y, if selected { ">" } else { " " }, Style::new());
        buf.set_string(inner.x + 1, y, cut(&k.name.to_uppercase(), name_w), Style::new());
        let status = status_of(k);
        let style = if status == "UP" {
            Style::new()
        } else {
            Style::new().add_modifier(Modifier::REVERSED)
        };
        buf.set_string(status_x, y, cut(&status, status_w), style);
        if selected {
            buf.set_style(Rect::new(inner.x + 1, y, inner.width - 1, 1), Style::new().add_modifier(Modifier::UNDERLINED));
        }
    }
    let (text, reverse) = message;
    let style = if reverse {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new()
    };
    let bottom = inner.y + inner.height;
    buf.set_string(inner.x, bottom - 2, cut(text, w), style);
    buf.set_string(inner.x, bottom - 1, cut(hint, w), Style::new());
}
