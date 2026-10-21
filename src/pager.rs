//! Page cache for one read. Earlier pages are served from memory, so going back is instant;
//! only moving past the last cached page needs a request.

use crate::ddb::{Cursor, Page};

#[derive(Default)]
pub struct Pager {
    pages: Vec<Page>,
    current: usize,
}

impl Pager {
    pub fn clear(&mut self) {
        self.pages.clear();
        self.current = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    pub fn page(&self) -> Option<&Page> {
        self.pages.get(self.current)
    }

    /// 1-based number of the current page.
    pub fn number(&self) -> usize {
        self.current + 1
    }

    /// Cursor for the page after the current one, if DynamoDB reported more.
    pub fn next_cursor(&self) -> Option<Cursor> {
        self.page()?.next.clone()
    }

    /// Cursor that started the current page; `None` for the first page.
    pub fn current_start(&self) -> Option<Cursor> {
        self.current
            .checked_sub(1)
            .and_then(|i| self.pages[i].next.clone())
    }

    pub fn has_next(&self) -> bool {
        self.current + 1 < self.pages.len() || self.next_cursor().is_some()
    }

    /// Moves forward when the next page is cached; `false` means it has to be fetched.
    pub fn next_cached(&mut self) -> bool {
        let cached = self.current + 1 < self.pages.len();
        if cached {
            self.current += 1;
        }
        cached
    }

    pub fn prev(&mut self) -> bool {
        let moved = self.current > 0;
        if moved {
            self.current -= 1;
        }
        moved
    }

    /// Appends a fetched page after the current one and moves to it.
    pub fn push(&mut self, page: Page) {
        if !self.pages.is_empty() {
            self.pages.truncate(self.current + 1);
        }
        self.pages.push(page);
        self.current = self.pages.len() - 1;
    }

    /// Replaces the current page after a refresh. Later pages are dropped because their
    /// boundaries may have shifted.
    pub fn replace_current(&mut self, page: Page) {
        if self.pages.is_empty() {
            self.pages.push(page);
            return;
        }
        self.pages.truncate(self.current + 1);
        self.pages[self.current] = page;
    }

    /// DynamoDB can report a continuation that turns out to be empty; stop offering it.
    pub fn mark_end(&mut self) {
        if let Some(p) = self.pages.get_mut(self.current) {
            p.next = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::Item;

    fn page(items: usize, next: Option<&str>) -> Page {
        Page {
            items: vec![Item::new(); items],
            next: next.map(|t| Cursor::Token(t.into())),
            consumed: None,
            calls: Vec::new(),
        }
    }

    fn token(t: &str) -> Option<Cursor> {
        Some(Cursor::Token(t.into()))
    }

    #[test]
    fn walks_back_and_forth_through_cached_pages() {
        let mut p = Pager::default();
        p.push(page(2, Some("a")));
        assert!(p.has_next() && !p.next_cached());
        assert_eq!(p.next_cursor(), token("a"));
        assert_eq!(p.current_start(), None);

        p.push(page(1, None));
        assert_eq!(p.number(), 2);
        assert!(!p.has_next());
        assert_eq!(p.current_start(), token("a"));

        assert!(p.prev());
        assert!(!p.prev());
        assert!(p.next_cached());
        assert_eq!(p.page().unwrap().items.len(), 1);
    }

    #[test]
    fn refresh_replaces_current_and_drops_later_pages() {
        let mut p = Pager::default();
        p.push(page(1, Some("a")));
        p.push(page(1, Some("b")));
        p.prev();
        p.replace_current(page(3, Some("c")));
        assert_eq!(p.page().unwrap().items.len(), 3);
        assert!(!p.next_cached());
        assert_eq!(p.next_cursor(), token("c"));
    }

    #[test]
    fn mark_end_stops_paging() {
        let mut p = Pager::default();
        p.push(page(1, Some("a")));
        p.mark_end();
        assert!(!p.has_next());
    }
}
