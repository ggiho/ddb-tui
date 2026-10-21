//! Page cache for one read. Every page keeps its cursors, so any earlier page can be read
//! again, but only pages near the current one keep their items: a page can hold 500 items of
//! up to 400 KB each, so caching all of them would grow without bound while paging through a
//! large table. Going back to a page whose items were dropped reads it again.

use crate::ddb::{Cursor, Page};
use crate::item;

/// Pages besides the current one that keep their items.
const KEEP_PAGES: usize = 10;
/// Item data those pages may hold together, by DynamoDB's own size measure.
const KEEP_BYTES: usize = 32 << 20;

struct Slot {
    page: Page,
    /// Whether the items are still here; a dropped page keeps its cursors only.
    loaded: bool,
    bytes: usize,
}

#[derive(Default)]
pub struct Pager {
    slots: Vec<Slot>,
    current: usize,
}

impl Pager {
    pub fn clear(&mut self) {
        self.slots.clear();
        self.current = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// The current page, unless its items were dropped and are being read again.
    pub fn page(&self) -> Option<&Page> {
        self.slots
            .get(self.current)
            .filter(|s| s.loaded)
            .map(|s| &s.page)
    }

    /// Whether the current page's items were dropped and have to be read again from
    /// `current_start`.
    pub fn needs_fetch(&self) -> bool {
        self.slots.get(self.current).is_some_and(|s| !s.loaded)
    }

    /// 1-based number of the current page.
    pub fn number(&self) -> usize {
        self.current + 1
    }

    /// Cursor for the page after the current one, if DynamoDB reported more.
    pub fn next_cursor(&self) -> Option<Cursor> {
        self.slots.get(self.current)?.page.next.clone()
    }

    /// Cursor that started the current page; `None` for the first page.
    pub fn current_start(&self) -> Option<Cursor> {
        self.current
            .checked_sub(1)
            .and_then(|i| self.slots[i].page.next.clone())
    }

    pub fn has_next(&self) -> bool {
        self.current + 1 < self.slots.len() || self.next_cursor().is_some()
    }

    /// Moves forward when the next page is known; `false` means it has to be fetched.
    pub fn next_cached(&mut self) -> bool {
        let known = self.current + 1 < self.slots.len();
        if known {
            self.current += 1;
        }
        known
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
        if !self.slots.is_empty() {
            self.slots.truncate(self.current + 1);
        }
        self.slots.push(slot(page));
        self.current = self.slots.len() - 1;
        self.trim();
    }

    /// Replaces the current page after a refresh or a re-read. Later pages are dropped
    /// because their boundaries may have shifted.
    pub fn replace_current(&mut self, page: Page) {
        if self.slots.is_empty() {
            self.slots.push(slot(page));
            return;
        }
        // Later pages stay when this page still ends where it did, as when a dropped page is
        // read again; otherwise their boundaries may have shifted.
        if self.slots[self.current].page.next != page.next {
            self.slots.truncate(self.current + 1);
        }
        self.slots[self.current] = slot(page);
        self.trim();
    }

    /// DynamoDB can report a continuation that turns out to be empty; stop offering it.
    pub fn mark_end(&mut self) {
        if let Some(s) = self.slots.get_mut(self.current) {
            s.page.next = None;
        }
    }

    /// Drops the items of the pages farthest from the current one until the others fit in
    /// `KEEP_PAGES` and `KEEP_BYTES`.
    fn trim(&mut self) {
        let current = self.current;
        let mut others: Vec<usize> = (0..self.slots.len())
            .filter(|&i| i != current && self.slots[i].loaded)
            .collect();
        others.sort_by_key(|&i| std::cmp::Reverse(i.abs_diff(current)));
        let mut count = others.len();
        let mut bytes: usize = others.iter().map(|&i| self.slots[i].bytes).sum();
        for i in others {
            if count <= KEEP_PAGES && bytes <= KEEP_BYTES {
                break;
            }
            let s = &mut self.slots[i];
            s.loaded = false;
            s.page.items = Vec::new();
            count -= 1;
            bytes -= s.bytes;
        }
    }
}

fn slot(page: Page) -> Slot {
    let bytes = page.items.iter().map(item::approx_size).sum();
    Slot {
        page,
        loaded: true,
        bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::Item;
    use aws_sdk_dynamodb::types::AttributeValue;

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

    #[test]
    fn reading_a_page_again_keeps_the_pages_after_it() {
        let mut p = Pager::default();
        p.push(page(1, Some("a")));
        p.push(page(1, Some("b")));
        p.prev();
        p.replace_current(page(2, Some("a")));
        assert!(p.next_cached(), "same boundary: page 2 is still known");
    }

    #[test]
    fn far_pages_drop_their_items_but_keep_their_cursors() {
        let mut p = Pager::default();
        for i in 0..KEEP_PAGES + 3 {
            p.push(page(1, Some(&format!("after{i}"))));
        }
        while p.prev() {}
        assert!(p.needs_fetch(), "the first page was dropped");
        assert!(p.page().is_none());
        assert_eq!(p.current_start(), None, "read again from the start");
        assert!(p.next_cached());
        assert_eq!(p.current_start(), token("after0"), "cursors survive");
        p.replace_current(page(1, Some("after1")));
        assert!(p.page().is_some(), "loaded again");
    }

    #[test]
    fn large_pages_are_kept_within_the_byte_budget() {
        let big = Item::from([(
            "blob".to_string(),
            AttributeValue::S("x".repeat(KEEP_BYTES / 2 + 1)),
        )]);
        let mut p = Pager::default();
        for i in 0..3 {
            let mut pg = page(0, Some(&format!("n{i}")));
            pg.items = vec![big.clone()];
            p.push(pg);
        }
        p.prev();
        assert!(p.page().is_some(), "the nearest page fits");
        p.prev();
        assert!(
            p.needs_fetch(),
            "two big pages besides the current one do not"
        );
    }
}
