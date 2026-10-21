//! Application state and key handling. AWS calls run on the Tokio runtime and report back
//! as `Msg`, so the UI never waits on the network.

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::primitives::Blob;
use aws_sdk_dynamodb::types::AttributeValue;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::{ListState, TableState};
use tokio::runtime::Handle;
use tokio::sync::mpsc::UnboundedSender;

use crate::complete;
use crate::ddb::{self, Connection, Cursor, DeleteReport, Page, ReadRequest, TableInfo};
use crate::details;
use crate::export;
use crate::expr::Placeholders;
use crate::form::{Field, ReadForm};
use crate::item::{self, Item};
use crate::pager::Pager;
use crate::saved;

/// Page sizes offered by `+`/`-`; the second is the default.
pub const PAGE_SIZES: [usize; 5] = [25, 50, 100, 200, 500];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Connections,
    Tables,
    Regions,
    Items,
    Detail,
    Form,
    PartiQL,
    /// UpdateItem expression popup.
    Update,
    /// Saved operations of the current connection and region.
    Saved,
}

pub enum Msg {
    Connected {
        conn: Connection,
        client: Client,
        region: String,
        tables: anyhow::Result<Vec<String>>,
    },
    Tables(anyhow::Result<Vec<String>>),
    Described(anyhow::Result<TableInfo>),
    Page {
        generation: u64,
        replace: bool,
        result: anyhow::Result<Page>,
    },
    /// Write capacity used by a PutItem; `created` when it wrote a new item.
    Saved {
        created: bool,
        result: anyhow::Result<Option<f64>>,
    },
    Deleted(DeleteReport),
    Details {
        table: String,
        result: anyhow::Result<String>,
    },
    /// The updated item (ReturnValues ALL_NEW) and the write capacity used.
    Updated(anyhow::Result<(Option<Item>, Option<f64>)>),
    /// The full item fetched for the editor, or `None` if it no longer exists.
    Loaded {
        kind: EditKind,
        result: anyhow::Result<Option<Item>>,
    },
}

pub enum Status {
    Info(String),
    Error(String),
}

/// What the external editor is being used for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    /// Change an existing item in place.
    Edit,
    /// Start from a skeleton holding only the key attributes.
    Create,
    /// Start from a copy of an existing item.
    Duplicate,
}

pub enum Confirm {
    /// `create`: the write must not replace an existing item.
    Save {
        table: String,
        item: Item,
        create: bool,
        key_changed: bool,
    },
    Delete {
        table: String,
        keys: Vec<Item>,
    },
    Update(ddb::UpdateRequest),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptAction {
    Export,
    SaveOperation,
}

/// A one-line text prompt: the export file name or a saved operation name.
pub struct Prompt {
    pub title: String,
    pub text: String,
    pub action: PromptAction,
}

/// The UpdateItem popup. Texts are kept between uses, so one update can be applied to
/// several items in turn.
#[derive(Default)]
pub struct UpdateForm {
    pub update: String,
    pub condition: String,
    pub on_condition: bool,
}

pub struct App {
    pub screen: Screen,
    pub connections: Vec<Connection>,
    pub conn_state: ListState,
    pub conn_label: String,
    pub region: String,
    pub region_state: ListState,
    pub tables: Vec<String>,
    pub filter: String,
    pub filtering: bool,
    pub table_state: ListState,
    pub table: Option<TableInfo>,
    pub request: Option<ReadRequest>,
    pub pager: Pager,
    pub item_state: TableState,
    /// Horizontal scroll over the non-key columns; key columns stay pinned on the left.
    pub col_offset: usize,
    pub page_size: usize,
    /// Rows of the current page marked for delete or export.
    pub marked: BTreeSet<usize>,
    pub detail: Option<Item>,
    pub detail_title: String,
    pub detail_text: String,
    pub detail_scroll: u16,
    pub form: ReadForm,
    pub partiql: String,
    pub confirm: Option<Confirm>,
    pub prompt: Option<Prompt>,
    pub update_form: UpdateForm,
    /// Attribute names offered by Tab completion when there is more than one.
    pub hint: Vec<String>,
    pub saved: Vec<saved::Entry>,
    pub saved_state: ListState,
    pub show_help: bool,
    pub status: Status,
    /// Requests in flight; drives the loading indicator.
    pub busy: u32,
    pub quit: bool,
    client: Option<Client>,
    connection: Option<Connection>,
    partiql_return: Screen,
    detail_return: Screen,
    edit_request: Option<(EditKind, Item)>,
    editing: Option<EditKind>,
    saving: Option<Item>,
    clipboard: Option<String>,
    /// DescribeTable results of the current connection, so reopening a table is instant.
    table_cache: HashMap<String, TableInfo>,
    /// Attribute names seen per table, for completion.
    seen_attrs: HashMap<String, BTreeSet<String>>,
    /// The form behind the current Scan/Query, which is what a saved operation stores.
    last_form: Option<ReadForm>,
    update_target: Option<(String, Item)>,
    pub update_return: Screen,
    saved_return: Screen,
    /// A saved Scan/Query waiting for its table's key schema.
    pending_saved: Option<saved::SavedRead>,
    /// Bumped whenever a page result would no longer apply, so late responses are dropped.
    generation: u64,
    rt: Handle,
    tx: UnboundedSender<Msg>,
}

impl App {
    pub fn new(connections: Vec<Connection>, rt: Handle, tx: UnboundedSender<Msg>) -> Self {
        Self {
            screen: Screen::Connections,
            conn_state: ListState::default().with_selected((!connections.is_empty()).then_some(0)),
            connections,
            conn_label: String::new(),
            region: String::new(),
            region_state: ListState::default(),
            tables: Vec::new(),
            filter: String::new(),
            filtering: false,
            table_state: ListState::default(),
            table: None,
            request: None,
            pager: Pager::default(),
            item_state: TableState::default(),
            col_offset: 0,
            page_size: PAGE_SIZES[1],
            marked: BTreeSet::new(),
            detail: None,
            detail_title: String::new(),
            detail_text: String::new(),
            detail_scroll: 0,
            form: ReadForm::default(),
            partiql: String::new(),
            confirm: None,
            prompt: None,
            update_form: UpdateForm::default(),
            hint: Vec::new(),
            saved: Vec::new(),
            saved_state: ListState::default(),
            show_help: false,
            status: Status::Info(String::new()),
            busy: 0,
            quit: false,
            client: None,
            connection: None,
            partiql_return: Screen::Tables,
            detail_return: Screen::Items,
            edit_request: None,
            editing: None,
            saving: None,
            clipboard: None,
            table_cache: HashMap::new(),
            seen_attrs: HashMap::new(),
            last_form: None,
            update_target: None,
            update_return: Screen::Items,
            saved_return: Screen::Tables,
            pending_saved: None,
            generation: 0,
            rt,
            tx,
        }
    }

    pub fn visible_tables(&self) -> Vec<&str> {
        let filter = self.filter.to_lowercase();
        self.tables
            .iter()
            .filter(|t| t.to_lowercase().contains(&filter))
            .map(String::as_str)
            .collect()
    }

    pub fn items(&self) -> &[Item] {
        self.pager.page().map(|p| p.items.as_slice()).unwrap_or(&[])
    }

    fn selected_item(&self) -> Option<&Item> {
        self.item_state.selected().and_then(|i| self.items().get(i))
    }

    /// Marked rows, or the selected row when nothing is marked.
    fn target_rows(&self) -> Vec<Item> {
        if self.marked.is_empty() {
            self.selected_item().cloned().into_iter().collect()
        } else {
            self.marked
                .iter()
                .filter_map(|&i| self.items().get(i).cloned())
                .collect()
        }
    }

    fn selected_table(&self) -> Option<String> {
        let i = self.table_state.selected()?;
        self.visible_tables().get(i).map(|s| s.to_string())
    }

    fn request_table(&self) -> Option<String> {
        self.request
            .as_ref()
            .and_then(|r| r.table())
            .map(str::to_string)
    }

    fn is_partiql(&self) -> bool {
        matches!(self.request, Some(ReadRequest::PartiQL { .. }))
    }

    /// Column names and how many leading ones are key attributes: the table's keys, then the
    /// read index's keys, then everything else alphabetically. Keys a projection left out of
    /// every item are skipped.
    pub fn columns(&self) -> (Vec<String>, usize) {
        let mut cols: Vec<String> = Vec::new();
        if let (Some(t), Some(req)) = (&self.table, &self.request)
            && req.table() == Some(t.name.as_str())
        {
            let read_index = req
                .index()
                .and_then(|n| t.indexes.iter().find(|i| i.name.as_deref() == Some(n)));
            for idx in std::iter::once(t.base()).chain(read_index) {
                for k in idx.keys() {
                    let present = self.items().iter().any(|i| i.contains_key(&k.name));
                    if present && !cols.contains(&k.name) {
                        cols.push(k.name.clone());
                    }
                }
            }
        }
        let keys = cols.len();
        let rest: BTreeSet<&String> = self
            .items()
            .iter()
            .flat_map(|i| i.keys())
            .filter(|k| !cols.contains(k))
            .collect();
        cols.extend(rest.into_iter().cloned());
        (cols, keys)
    }

    /// An item the main loop should open in the external editor.
    pub fn take_edit_request(&mut self) -> Option<Item> {
        let (kind, item) = self.edit_request.take()?;
        self.editing = Some(kind);
        Some(item)
    }

    /// Called after the external editor returns; `Ok(None)` means the item was not changed.
    pub fn on_edited(&mut self, original: Item, result: anyhow::Result<Option<Item>>) {
        let kind = self.editing.take().unwrap_or(EditKind::Edit);
        match result {
            Err(e) => self.error(e),
            Ok(None) if kind == EditKind::Duplicate => {
                self.info("Unchanged; change the key to make a copy")
            }
            Ok(None) => self.info("No changes"),
            Ok(Some(item)) => {
                let Some(table) = self.request_table() else {
                    return;
                };
                let key_changed = self.table.as_ref().is_some_and(|t| {
                    t.base()
                        .keys()
                        .any(|k| original.get(&k.name) != item.get(&k.name))
                });
                self.confirm = Some(Confirm::Save {
                    table,
                    item,
                    create: kind != EditKind::Edit || key_changed,
                    key_changed: kind == EditKind::Edit && key_changed,
                });
            }
        }
    }

    /// Text the main loop should put on the clipboard.
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.clipboard.take()
    }

    /// Pasted text goes into the focused text input. Inputs are single-line, so newlines become
    /// spaces; a multi-line paste never runs a statement half-way.
    pub fn on_paste(&mut self, text: &str) {
        let text = text.replace("\r\n", " ").replace(['\r', '\n'], " ");
        if let Some(prompt) = self.prompt.as_mut() {
            prompt.text.push_str(&text);
            return;
        }
        if self.confirm.is_some() || self.show_help {
            return;
        }
        match self.screen {
            Screen::PartiQL => self.partiql.push_str(&text),
            Screen::Form => {
                if let Some(table) = self.table.clone()
                    && let Some(field) = self.form.text_mut(&table)
                {
                    field.push_str(&text);
                }
            }
            Screen::Tables if self.filtering => {
                self.filter.push_str(&text);
                self.select_first_table();
            }
            _ => {}
        }
    }

    pub fn on_copied(&mut self, result: anyhow::Result<&'static str>) {
        match result {
            Ok(how) => self.info(format!("Copied ({how})")),
            Err(e) => self.error(e),
        }
    }

    pub fn on_msg(&mut self, msg: Msg) {
        self.busy = self.busy.saturating_sub(1);
        match msg {
            Msg::Connected {
                conn,
                client,
                region,
                tables,
            } => match tables {
                Ok(tables) => {
                    self.client = Some(client);
                    self.table_cache.clear();
                    self.seen_attrs.clear();
                    self.conn_label = conn.label();
                    self.connection = Some(conn);
                    self.region = region;
                    self.set_tables(tables);
                    self.screen = Screen::Tables;
                }
                Err(e) => self.error(e),
            },
            Msg::Tables(result) => match result {
                Ok(tables) => self.set_tables(tables),
                Err(e) => self.error(e),
            },
            Msg::Described(result) => match result {
                Ok(t) => {
                    self.table_cache.insert(t.name.clone(), t.clone());
                    let keys = t
                        .indexes
                        .iter()
                        .flat_map(|i| i.keys().map(|k| k.name.clone()));
                    self.seen_attrs
                        .entry(t.name.clone())
                        .or_default()
                        .extend(keys);
                    if self
                        .pending_saved
                        .as_ref()
                        .is_some_and(|p| p.table == t.name)
                    {
                        let read = self.pending_saved.take().expect("checked");
                        self.run_saved_read(read, t);
                    } else if self.request_table().as_deref() == Some(t.name.as_str()) {
                        self.table = Some(t);
                    }
                }
                Err(e) => {
                    self.pending_saved = None;
                    self.error(e);
                }
            },
            Msg::Page {
                generation,
                replace,
                result,
            } => {
                if generation != self.generation {
                    return;
                }
                match result {
                    Ok(page) => self.on_page(page, replace),
                    Err(e) => self.error(e),
                }
            }
            Msg::Saved { created, result } => {
                let saved = self.saving.take();
                match result {
                    Ok(units) => {
                        let what = if created { "Created" } else { "Saved" };
                        self.info(format!("{what}{}", wcu(units)));
                        if let Some(item) = saved
                            && !created
                            && self.screen == Screen::Detail
                        {
                            self.set_detail(item);
                        }
                        self.refresh();
                    }
                    Err(e) => self.error(e),
                }
            }
            Msg::Deleted(report) => {
                self.marked.clear();
                if report.failed.is_empty() {
                    self.info(format!(
                        "Deleted {} item(s){}",
                        report.deleted,
                        wcu(report.consumed)
                    ));
                } else {
                    self.error(format!(
                        "Deleted {} of {}; {}",
                        report.deleted,
                        report.requested,
                        report.failed.join("; ")
                    ));
                }
                self.refresh();
            }
            Msg::Details { table, result } => match result {
                Ok(text) => {
                    self.info("");
                    self.detail = None;
                    self.detail_title = format!(" {table} · table details ");
                    self.detail_text = text;
                    self.detail_scroll = 0;
                    self.detail_return = self.screen;
                    self.screen = Screen::Detail;
                }
                Err(e) => self.error(e),
            },
            Msg::Updated(result) => match result {
                Ok((item, units)) => {
                    self.info(format!("Updated{}", wcu(units)));
                    if let Some(item) = item
                        && self.screen == Screen::Detail
                        && self.detail.is_some()
                    {
                        self.set_detail(item);
                    }
                    self.refresh();
                }
                Err(e) => self.error(e),
            },
            Msg::Loaded { kind, result } => match result {
                Ok(Some(item)) => {
                    self.info("");
                    self.edit_request = Some((kind, item));
                }
                Ok(None) => self.info("The item no longer exists"),
                Err(e) => self.error(e),
            },
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.show_help {
            self.show_help = false;
            return;
        }
        if self.prompt.is_some() {
            self.key_prompt(key.code);
            return;
        }
        if self.confirm.is_some() {
            self.key_confirm(key.code);
            return;
        }
        match self.screen {
            Screen::Connections => self.key_connections(key.code),
            Screen::Tables => self.key_tables(key.code),
            Screen::Regions => self.key_regions(key.code),
            Screen::Items => self.key_items(key.code),
            Screen::Detail => self.key_detail(key.code),
            Screen::Form => self.key_form(key.code),
            Screen::PartiQL => self.key_partiql(key.code),
            Screen::Update => self.key_update(key.code),
            Screen::Saved => self.key_saved(key.code),
        }
    }

    fn key_confirm(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('y') => match self.confirm.take() {
                Some(Confirm::Save {
                    table,
                    item,
                    create,
                    ..
                }) => self.save(table, item, create),
                Some(Confirm::Delete { table, keys }) => self.delete(table, keys),
                Some(Confirm::Update(req)) => self.update(req),
                None => {}
            },
            KeyCode::Char('n') | KeyCode::Esc => {
                self.confirm = None;
                self.info("Cancelled");
            }
            _ => {}
        }
    }

    fn key_prompt(&mut self, code: KeyCode) {
        let Some(prompt) = self.prompt.as_mut() else {
            return;
        };
        match code {
            KeyCode::Esc => {
                self.prompt = None;
                self.info("Cancelled");
            }
            KeyCode::Enter => {
                let text = prompt.text.trim().to_string();
                let action = prompt.action;
                self.prompt = None;
                match action {
                    PromptAction::Export => self.export(&text),
                    PromptAction::SaveOperation => self.save_operation(&text),
                }
            }
            KeyCode::Char(c) => prompt.text.push(c),
            KeyCode::Backspace => {
                prompt.text.pop();
            }
            _ => {}
        }
    }

    fn key_connections(&mut self, code: KeyCode) {
        let len = self.connections.len();
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.conn_state
                    .select(step(self.conn_state.selected(), len, -1))
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.conn_state
                    .select(step(self.conn_state.selected(), len, 1))
            }
            KeyCode::Enter => self.connect(),
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            _ => {}
        }
    }

    fn key_tables(&mut self, code: KeyCode) {
        let len = self.visible_tables().len();
        match code {
            KeyCode::Up => self
                .table_state
                .select(step(self.table_state.selected(), len, -1)),
            KeyCode::Down => self
                .table_state
                .select(step(self.table_state.selected(), len, 1)),
            KeyCode::Enter => {
                self.filtering = false;
                if let Some(name) = self.selected_table() {
                    self.open_table(name);
                }
            }
            _ if self.filtering => match code {
                KeyCode::Char(c) => {
                    self.filter.push(c);
                    self.select_first_table();
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.select_first_table();
                }
                KeyCode::Esc => {
                    self.filter.clear();
                    self.filtering = false;
                    self.select_first_table();
                }
                _ => {}
            },
            KeyCode::Char('k') => {
                self.table_state
                    .select(step(self.table_state.selected(), len, -1))
            }
            KeyCode::Char('j') => {
                self.table_state
                    .select(step(self.table_state.selected(), len, 1))
            }
            // A new search, like `/` in less or vim; the last filter stays until then.
            KeyCode::Char('/') => {
                self.filter.clear();
                self.filtering = true;
                self.select_first_table();
            }
            KeyCode::Char('x') => {
                if let Some(name) = self.selected_table() {
                    self.partiql = format!("SELECT * FROM \"{name}\"");
                }
                self.open_partiql(Screen::Tables);
            }
            KeyCode::Char('R') => self.open_regions(),
            KeyCode::Char('o') => self.open_saved(Screen::Tables),
            KeyCode::Char('t') => {
                if let Some(table) = self.selected_table() {
                    self.open_details(table);
                }
            }
            KeyCode::Char('r') => self.refresh_tables(),
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('q') | KeyCode::Esc => self.screen = Screen::Connections,
            _ => {}
        }
    }

    fn key_regions(&mut self, code: KeyCode) {
        let len = ddb::REGIONS.len();
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.region_state
                    .select(step(self.region_state.selected(), len, -1))
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.region_state
                    .select(step(self.region_state.selected(), len, 1))
            }
            KeyCode::Enter => self.switch_region(),
            KeyCode::Char('q') | KeyCode::Esc => self.screen = Screen::Tables,
            _ => {}
        }
    }

    fn key_items(&mut self, code: KeyCode) {
        let n = self.items().len();
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.item_state
                    .select(step(self.item_state.selected(), n, -1))
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.item_state
                    .select(step(self.item_state.selected(), n, 1))
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.col_offset = self.col_offset.saturating_sub(1)
            }
            KeyCode::Right | KeyCode::Char('l') => {
                let (cols, keys) = self.columns();
                if self.col_offset + 1 < cols.len() - keys {
                    self.col_offset += 1;
                }
            }
            KeyCode::Char('n') | KeyCode::Char(']') => self.next_page(),
            KeyCode::Char('p') | KeyCode::Char('[') => self.prev_page(),
            KeyCode::Char(' ') => self.toggle_mark(),
            KeyCode::Enter => {
                if let Some(item) = self.selected_item().cloned() {
                    self.set_detail(item);
                    self.detail_return = Screen::Items;
                    self.screen = Screen::Detail;
                }
            }
            KeyCode::Char('e') => {
                if let Some(item) = self.selected_item().cloned() {
                    self.load_for_editor(item, EditKind::Edit);
                }
            }
            KeyCode::Char('c') => {
                if let Some(item) = self.selected_item().cloned() {
                    self.load_for_editor(item, EditKind::Duplicate);
                }
            }
            KeyCode::Char('a') => self.request_create(),
            KeyCode::Char('d') => self.request_delete(),
            KeyCode::Char('y') => self.copy(self.selected_item().cloned(), false),
            KeyCode::Char('Y') => self.copy(self.selected_item().cloned(), true),
            KeyCode::Char('E') => self.open_export(),
            KeyCode::Char('u') => {
                if let Some(item) = self.selected_item().cloned() {
                    self.open_update(item, Screen::Items);
                }
            }
            KeyCode::Char('w') => self.open_save_prompt(),
            KeyCode::Char('o') => self.open_saved(Screen::Items),
            KeyCode::Char('i') => self.inspect_page(),
            KeyCode::Char('+') | KeyCode::Char('=') => self.change_page_size(1),
            KeyCode::Char('-') => self.change_page_size(-1),
            KeyCode::Char('t') => {
                if let Some(table) = self.request_table() {
                    self.open_details(table);
                }
            }
            KeyCode::Char('f') => self.open_form(),
            KeyCode::Char('s') => {
                if let Some(table) = self.request_table() {
                    self.last_form = Some(scan_form());
                    self.start_read(ReadRequest::scan(table));
                }
            }
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Char('x') => {
                if !self.is_partiql()
                    && let Some(table) = self.request_table()
                {
                    self.partiql = format!("SELECT * FROM \"{table}\"");
                }
                self.open_partiql(Screen::Items);
            }
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Esc | KeyCode::Char('q') => {
                self.generation += 1;
                self.screen = if self.is_partiql() {
                    Screen::PartiQL
                } else {
                    Screen::Tables
                };
            }
            _ => {}
        }
    }

    fn key_detail(&mut self, code: KeyCode) {
        let max = self.detail_text.lines().count().saturating_sub(1) as u16;
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.detail_scroll = (self.detail_scroll + 1).min(max)
            }
            KeyCode::PageUp => self.detail_scroll = self.detail_scroll.saturating_sub(20),
            KeyCode::PageDown => self.detail_scroll = (self.detail_scroll + 20).min(max),
            KeyCode::Char('e') => {
                if let Some(item) = self.detail.clone() {
                    self.load_for_editor(item, EditKind::Edit);
                }
            }
            KeyCode::Char('c') => {
                if let Some(item) = self.detail.clone() {
                    self.load_for_editor(item, EditKind::Duplicate);
                }
            }
            KeyCode::Char('u') => {
                if let Some(item) = self.detail.clone() {
                    self.open_update(item, Screen::Detail);
                }
            }
            KeyCode::Char('y') => self.copy(self.detail.clone(), false),
            KeyCode::Char('Y') => self.copy(self.detail.clone(), true),
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Esc | KeyCode::Char('q') => self.screen = self.detail_return,
            _ => {}
        }
    }

    fn key_form(&mut self, code: KeyCode) {
        let Some(table) = self.table.clone() else {
            self.screen = Screen::Items;
            return;
        };
        if code != KeyCode::Tab {
            self.hint.clear();
        }
        match code {
            KeyCode::Esc => self.screen = Screen::Items,
            KeyCode::Tab => {
                let names = match self.form.active(&table) {
                    Field::Filter | Field::Projection => self.completion_names(),
                    _ => BTreeSet::new(),
                };
                let completed = self
                    .form
                    .text_mut(&table)
                    .is_some_and(|field| complete_into(&names, field, &mut self.hint));
                if !completed {
                    self.hint.clear();
                    self.form.move_focus(&table, 1);
                }
            }
            KeyCode::Down => self.form.move_focus(&table, 1),
            KeyCode::BackTab | KeyCode::Up => self.form.move_focus(&table, -1),
            KeyCode::Left => self.form.change(&table, -1),
            KeyCode::Right => self.form.change(&table, 1),
            KeyCode::Enter => match self.form.request(&table) {
                Ok(req) => {
                    self.last_form = Some(self.form.clone());
                    self.start_read(req);
                    self.screen = Screen::Items;
                }
                Err(e) => self.error(e),
            },
            KeyCode::Char(c) => {
                if let Some(text) = self.form.text_mut(&table) {
                    text.push(c);
                }
            }
            KeyCode::Backspace => {
                if let Some(text) = self.form.text_mut(&table) {
                    text.pop();
                }
            }
            _ => {}
        }
    }

    fn key_partiql(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.screen = self.partiql_return,
            KeyCode::Enter => {
                let statement = self.partiql.trim().to_string();
                if !statement.is_empty() {
                    self.partiql_return = Screen::Tables;
                    self.start_read(ReadRequest::PartiQL { statement });
                    self.screen = Screen::Items;
                }
            }
            KeyCode::Char(c) => self.partiql.push(c),
            KeyCode::Backspace => {
                self.partiql.pop();
            }
            _ => {}
        }
    }

    fn spawn<F>(&mut self, fut: F)
    where
        F: Future<Output = Msg> + Send + 'static,
    {
        self.busy += 1;
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let _ = tx.send(fut.await);
        });
    }

    fn connect(&mut self) {
        let Some(conn) = self
            .conn_state
            .selected()
            .and_then(|i| self.connections.get(i))
            .cloned()
        else {
            return;
        };
        self.info(format!("Connecting to {}…", conn.label()));
        self.spawn(async move {
            let (client, region) = ddb::connect(&conn, None).await;
            let tables = ddb::list_tables(&client).await;
            Msg::Connected {
                conn,
                client,
                region,
                tables,
            }
        });
    }

    fn open_regions(&mut self) {
        match &self.connection {
            Some(Connection::Profile(_)) => {
                let current = ddb::REGIONS.iter().position(|r| *r == self.region);
                self.region_state.select(current.or(Some(0)));
                self.screen = Screen::Regions;
            }
            Some(Connection::Local(_)) => self.info("DynamoDB Local ignores regions"),
            None => {}
        }
    }

    /// Reconnects the current profile to another region and reloads its tables.
    fn switch_region(&mut self) {
        let (Some(conn), Some(i)) = (self.connection.clone(), self.region_state.selected()) else {
            return;
        };
        let region = ddb::REGIONS[i].to_string();
        self.info(format!("Switching to {region}…"));
        self.screen = Screen::Tables;
        self.spawn(async move {
            let (client, region) = ddb::connect(&conn, Some(&region)).await;
            let tables = ddb::list_tables(&client).await;
            Msg::Connected {
                conn,
                client,
                region,
                tables,
            }
        });
    }

    fn refresh_tables(&mut self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.table_cache.clear();
        self.spawn(async move { Msg::Tables(ddb::list_tables(&client).await) });
    }

    fn set_tables(&mut self, tables: Vec<String>) {
        self.tables = tables;
        self.filter.clear();
        self.filtering = false;
        self.select_first_table();
        self.info(format!("{} tables", self.tables.len()));
    }

    fn select_first_table(&mut self) {
        let any = !self.visible_tables().is_empty();
        self.table_state.select(any.then_some(0));
    }

    /// Starts the first scan page, describing the table at the same time unless its key
    /// schema is already cached.
    fn open_table(&mut self, name: String) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.form = ReadForm::default();
        self.table = self.table_cache.get(&name).cloned();
        if self.table.is_none() {
            let table = name.clone();
            self.spawn(async move { Msg::Described(ddb::describe(&client, &table).await) });
        }
        self.last_form = Some(scan_form());
        self.start_read(ReadRequest::scan(name));
        self.screen = Screen::Items;
    }

    fn open_form(&mut self) {
        match &self.table {
            Some(t) if self.request_table().as_deref() == Some(t.name.as_str()) => {
                self.screen = Screen::Form
            }
            _ if self.is_partiql() => {
                self.info("Scan/Query needs a table; open one from the table list")
            }
            _ => self.info("Table details are still loading"),
        }
    }

    fn open_partiql(&mut self, back: Screen) {
        self.partiql_return = back;
        self.screen = Screen::PartiQL;
    }

    fn start_read(&mut self, req: ReadRequest) {
        self.info("");
        self.request = Some(req);
        self.pager.clear();
        self.marked.clear();
        self.item_state.select(None);
        self.col_offset = 0;
        self.fetch(None, false);
    }

    fn fetch(&mut self, start: Option<Cursor>, replace: bool) {
        let (Some(client), Some(req)) = (self.client.clone(), self.request.clone()) else {
            return;
        };
        self.generation += 1;
        let generation = self.generation;
        let page_size = self.page_size;
        self.spawn(async move {
            Msg::Page {
                generation,
                replace,
                result: ddb::fetch_page(&client, &req, start, page_size).await,
            }
        });
    }

    fn refresh(&mut self) {
        if self.pager.is_empty() {
            self.fetch(None, false);
        } else {
            self.fetch(self.pager.current_start(), true);
        }
    }

    fn next_page(&mut self) {
        if self.pager.next_cached() {
            self.marked.clear();
            self.select_first_item();
            return;
        }
        match self.pager.next_cursor() {
            Some(cursor) => self.fetch(Some(cursor), false),
            None => self.info("Last page"),
        }
    }

    fn prev_page(&mut self) {
        // A next-page fetch still in flight would otherwise land after the wrong page.
        self.generation += 1;
        if self.pager.prev() {
            self.marked.clear();
            self.select_first_item();
        } else {
            self.info("First page");
        }
    }

    fn on_page(&mut self, page: Page, replace: bool) {
        if let Some(table) = self.request_table() {
            let names = page.items.iter().flat_map(|i| i.keys().cloned());
            self.seen_attrs.entry(table).or_default().extend(names);
        }
        // Row positions may have shifted, so marks no longer point at the same items.
        self.marked.clear();
        if replace {
            self.pager.replace_current(page);
            let n = self.items().len();
            self.item_state.select(
                self.item_state
                    .selected()
                    .map(|i| i.min(n.saturating_sub(1)))
                    .filter(|_| n > 0),
            );
        } else if !self.pager.is_empty() && page.items.is_empty() {
            self.pager.mark_end();
            self.info("No more items");
        } else {
            self.pager.push(page);
            self.select_first_item();
        }
    }

    fn select_first_item(&mut self) {
        let any = !self.items().is_empty();
        self.item_state.select(any.then_some(0));
    }

    fn toggle_mark(&mut self) {
        let Some(i) = self.item_state.selected() else {
            return;
        };
        if !self.marked.remove(&i) {
            self.marked.insert(i);
        }
        self.item_state.select(step(Some(i), self.items().len(), 1));
    }

    fn set_detail(&mut self, item: Item) {
        self.detail_title = " Item · DynamoDB JSON ".into();
        self.detail_text =
            serde_json::to_string_pretty(&item::item_to_json(&item)).unwrap_or_default();
        self.detail = Some(item);
        self.detail_scroll = 0;
    }

    /// Moves through `PAGE_SIZES` and restarts the read, since cached pages had the old size.
    fn change_page_size(&mut self, delta: isize) {
        let at = PAGE_SIZES
            .iter()
            .position(|&n| n == self.page_size)
            .unwrap_or(1);
        let next = (at as isize + delta).clamp(0, PAGE_SIZES.len() as isize - 1) as usize;
        if next == at {
            return;
        }
        self.page_size = PAGE_SIZES[next];
        if let Some(req) = self.request.clone() {
            self.start_read(req);
        }
        self.info(format!("Page size {}", self.page_size));
    }

    fn open_details(&mut self, table: String) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.info(format!("Loading details of {table}…"));
        self.spawn(async move {
            let result = details::report(&client, &table).await;
            Msg::Details { table, result }
        });
    }

    /// Shows the request parameters and response metadata of the calls behind this page.
    fn inspect_page(&mut self) {
        let Some(page) = self.pager.page() else {
            return;
        };
        self.detail_title = format!(
            " Page {} · {} call(s) · request and response metadata ",
            self.pager.number(),
            page.calls.len()
        );
        self.detail_text = serde_json::to_string_pretty(&page.calls).unwrap_or_default();
        self.detail = None;
        self.detail_scroll = 0;
        self.detail_return = Screen::Items;
        self.screen = Screen::Detail;
    }

    /// The table and its client when the current read can be written back; otherwise says why.
    fn writable(&mut self) -> Option<(String, TableInfo, Client)> {
        let Some(table) = self.request_table() else {
            self.info("Writes work on Scan/Query results; PartiQL results have no table");
            return None;
        };
        let Some(info) = self.table.clone().filter(|t| t.name == table) else {
            self.info("Table details are still loading");
            return None;
        };
        Some((table, info, self.client.clone()?))
    }

    /// The editor always starts from a fresh, complete copy: a row from a projection or a
    /// partially projected index would otherwise drop the attributes it does not show.
    fn load_for_editor(&mut self, item: Item, kind: EditKind) {
        let Some((table, info, client)) = self.writable() else {
            return;
        };
        let Some(key) = ddb::key_of(&item, info.base()) else {
            self.info("This row lacks the table's key attributes; include them in the projection");
            return;
        };
        self.info("Loading the full item…");
        self.spawn(async move {
            Msg::Loaded {
                kind,
                result: ddb::get_item(&client, &table, key).await,
            }
        });
    }

    fn request_create(&mut self) {
        let Some((_, info, _)) = self.writable() else {
            return;
        };
        let skeleton = info
            .base()
            .keys()
            .map(|k| (k.name.clone(), placeholder(&k.ty)))
            .collect();
        self.edit_request = Some((EditKind::Create, skeleton));
    }

    fn request_delete(&mut self) {
        let rows = self.target_rows();
        if rows.is_empty() {
            return;
        }
        let Some((table, info, _)) = self.writable() else {
            return;
        };
        let Some(keys) = rows
            .iter()
            .map(|r| ddb::key_of(r, info.base()))
            .collect::<Option<Vec<_>>>()
        else {
            self.info("Some rows lack the table's key attributes; include them in the projection");
            return;
        };
        self.confirm = Some(Confirm::Delete { table, keys });
    }

    fn save(&mut self, table: String, item: Item, create: bool) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let only_if_absent = match self.table.as_ref().filter(|t| t.name == table) {
            _ if !create => None,
            Some(info) => Some(info.base().pk.name.clone()),
            None => {
                self.info("Table details are still loading");
                return;
            }
        };
        self.saving = Some(item.clone());
        self.info("Saving…");
        self.spawn(async move {
            Msg::Saved {
                created: create,
                result: ddb::put_item(&client, &table, item, only_if_absent.as_deref()).await,
            }
        });
    }

    fn delete(&mut self, table: String, keys: Vec<Item>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.info(format!("Deleting {} item(s)…", keys.len()));
        self.spawn(async move { Msg::Deleted(ddb::delete_items(&client, &table, keys).await) });
    }

    fn copy(&mut self, item: Option<Item>, plain: bool) {
        let Some(item) = item else {
            return;
        };
        let value = if plain {
            item::item_to_plain_json(&item)
        } else {
            item::item_to_json(&item)
        };
        self.clipboard = Some(serde_json::to_string_pretty(&value).unwrap_or_default());
    }

    /// Marked rows, or the whole page when nothing is marked.
    fn export_rows(&self) -> Vec<Item> {
        if self.marked.is_empty() {
            self.items().to_vec()
        } else {
            self.target_rows()
        }
    }

    fn open_export(&mut self) {
        let n = self.export_rows().len();
        if n == 0 {
            return;
        }
        let name = self.request_table().unwrap_or_else(|| "partiql".into());
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let what = if self.marked.is_empty() {
            format!("page {} ({n} items)", self.pager.number())
        } else {
            format!("{n} marked items")
        };
        self.prompt = Some(Prompt {
            title: format!(" Export {what} · .csv or .json "),
            text: format!("{name}-{secs}.csv"),
            action: PromptAction::Export,
        });
    }

    /// Attribute names for completion: those seen in the current table plus its keys.
    fn completion_names(&self) -> BTreeSet<String> {
        let Some(table) = self.request_table() else {
            return BTreeSet::new();
        };
        let mut names = self.seen_attrs.get(&table).cloned().unwrap_or_default();
        if let Some(info) = self.table.as_ref().filter(|t| t.name == table) {
            for idx in &info.indexes {
                names.extend(idx.keys().map(|k| k.name.clone()));
            }
        }
        names
    }

    fn open_update(&mut self, item: Item, back: Screen) {
        let Some((table, info, _)) = self.writable() else {
            return;
        };
        let Some(key) = ddb::key_of(&item, info.base()) else {
            self.info("This row lacks the table's key attributes; include them in the projection");
            return;
        };
        self.update_target = Some((table, key));
        self.update_return = back;
        // Texts stay for the next item; focus always starts on the update expression.
        self.update_form.on_condition = false;
        self.hint.clear();
        self.screen = Screen::Update;
    }

    fn key_update(&mut self, code: KeyCode) {
        if code != KeyCode::Tab {
            self.hint.clear();
        }
        let form = &mut self.update_form;
        let field = if form.on_condition {
            &mut form.condition
        } else {
            &mut form.update
        };
        match code {
            KeyCode::Esc => self.screen = self.update_return,
            KeyCode::Tab => {
                let names = self.completion_names();
                let form = &mut self.update_form;
                let field = if form.on_condition {
                    &mut form.condition
                } else {
                    &mut form.update
                };
                if !complete_into(&names, field, &mut self.hint) {
                    self.hint.clear();
                    form.on_condition = !form.on_condition;
                }
            }
            KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
                form.on_condition = !form.on_condition
            }
            KeyCode::Char(c) => field.push(c),
            KeyCode::Backspace => {
                field.pop();
            }
            KeyCode::Enter => match self.update_request() {
                Ok(req) => self.confirm = Some(Confirm::Update(req)),
                Err(e) => self.error(e),
            },
            _ => {}
        }
    }

    fn update_request(&self) -> anyhow::Result<ddb::UpdateRequest> {
        use anyhow::Context;
        let (table, key) = self.update_target.clone().context("No item selected")?;
        let mut ph = Placeholders::default();
        let update = ph.update(&self.update_form.update).context("Update")?;
        let cond = self.update_form.condition.trim();
        let condition = (!cond.is_empty())
            .then(|| ph.condition(cond))
            .transpose()
            .context("Condition")?;
        let mut summary = self.update_form.update.trim().to_string();
        if !cond.is_empty() {
            summary.push_str(&format!("  if  {cond}"));
        }
        Ok(ddb::UpdateRequest {
            table,
            key,
            update,
            condition,
            names: ph.names,
            values: ph.values,
            summary,
        })
    }

    fn update(&mut self, req: ddb::UpdateRequest) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.screen = self.update_return;
        self.info("Updating…");
        self.spawn(async move { Msg::Updated(ddb::update_item(&client, req).await) });
    }

    /// Workbench keeps saved operations per connection name and region.
    fn saved_dir(&self) -> Option<PathBuf> {
        let connection = match self.connection.as_ref()? {
            Connection::Profile(p) => p.clone(),
            Connection::Local(_) => "local".to_string(),
        };
        saved::dir(&connection, &self.region)
    }

    fn open_saved(&mut self, back: Screen) {
        let Some(dir) = self.saved_dir() else {
            return;
        };
        self.saved = saved::list(&dir);
        if self.saved.is_empty() {
            self.info(format!("No saved operations in {}", dir.display()));
            return;
        }
        self.saved_state.select(Some(0));
        self.saved_return = back;
        self.screen = Screen::Saved;
    }

    fn key_saved(&mut self, code: KeyCode) {
        let len = self.saved.len();
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.saved_state
                    .select(step(self.saved_state.selected(), len, -1))
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.saved_state
                    .select(step(self.saved_state.selected(), len, 1))
            }
            KeyCode::Enter => {
                let Some(entry) = self.saved_state.selected().and_then(|i| self.saved.get(i))
                else {
                    return;
                };
                match saved::load(&entry.path) {
                    Ok(saved::Loaded::Read(read)) => self.apply_saved_read(read),
                    Ok(saved::Loaded::PartiQL(statement)) => {
                        self.partiql = statement.clone();
                        self.partiql_return = Screen::Tables;
                        self.start_read(ReadRequest::PartiQL { statement });
                        self.screen = Screen::Items;
                    }
                    Ok(saved::Loaded::Unsupported(why)) => self.info(why),
                    Err(e) => self.error(e),
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => self.screen = self.saved_return,
            _ => {}
        }
    }

    /// Runs a saved Scan/Query once its table's key schema is known.
    fn apply_saved_read(&mut self, read: saved::SavedRead) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if let Some(info) = self.table_cache.get(&read.table).cloned() {
            self.run_saved_read(read, info);
            return;
        }
        let table = read.table.clone();
        self.info(format!("Loading {table}…"));
        self.pending_saved = Some(read);
        self.spawn(async move { Msg::Described(ddb::describe(&client, &table).await) });
    }

    fn run_saved_read(&mut self, read: saved::SavedRead, info: TableInfo) {
        self.form = read.into_form(&info);
        self.table = Some(info.clone());
        match self.form.request(&info) {
            Ok(req) => {
                self.last_form = Some(self.form.clone());
                self.start_read(req);
                self.screen = Screen::Items;
            }
            Err(e) => self.error(e),
        }
    }

    fn open_save_prompt(&mut self) {
        let savable = match &self.request {
            Some(ReadRequest::PartiQL { .. }) => true,
            Some(_) => self.last_form.is_some(),
            None => false,
        };
        if !savable || self.saved_dir().is_none() {
            self.info("Nothing to save yet");
            return;
        }
        self.prompt = Some(Prompt {
            title: " Save this read as (Workbench saved operation) ".into(),
            text: String::new(),
            action: PromptAction::SaveOperation,
        });
    }

    fn save_operation(&mut self, name: &str) {
        let Some(dir) = self.saved_dir() else {
            return;
        };
        let result = match &self.request {
            Some(ReadRequest::PartiQL { statement }) => saved::save_partiql(&dir, name, statement),
            Some(req) => {
                let info = self
                    .table
                    .as_ref()
                    .filter(|t| Some(t.name.as_str()) == req.table());
                match (&self.last_form, info) {
                    (Some(form), Some(info)) => saved::save_read(&dir, name, info, form),
                    _ => Err(anyhow::anyhow!("Table details are still loading")),
                }
            }
            None => return,
        };
        match result {
            Ok(path) => self.info(format!("Saved to {}", path.display())),
            Err(e) => self.error(e),
        }
    }

    fn export(&mut self, path: &str) {
        let rows = self.export_rows();
        let (columns, _) = self.columns();
        match export::export(Path::new(path), &columns, &rows) {
            Ok(()) => self.info(format!("Exported {} items to {path}", rows.len())),
            Err(e) => self.error(e),
        }
    }

    fn info(&mut self, text: impl Into<String>) {
        self.status = Status::Info(text.into());
    }

    fn error(&mut self, e: impl std::fmt::Display) {
        self.status = Status::Error(format!("{e:#}"));
    }
}

/// The form a plain table scan corresponds to.
fn scan_form() -> ReadForm {
    let mut form = ReadForm::default();
    form.query = false;
    form
}

/// Tab in an expression field: completes the word when possible; `false` lets Tab move on.
fn complete_into(names: &BTreeSet<String>, field: &mut String, hint: &mut Vec<String>) -> bool {
    match complete::complete(field, names) {
        Some(c) => {
            *field = c.text;
            *hint = if c.matches.len() > 1 {
                c.matches
            } else {
                Vec::new()
            };
            true
        }
        None => false,
    }
}

fn wcu(units: Option<f64>) -> String {
    units.map(|u| format!(" · {u} WCU")).unwrap_or_default()
}

/// A value for a key attribute in the create skeleton, to be replaced in the editor.
fn placeholder(ty: &str) -> AttributeValue {
    match ty {
        "N" => AttributeValue::N("0".into()),
        "B" => AttributeValue::B(Blob::new(Vec::new())),
        _ => AttributeValue::S(String::new()),
    }
}

fn step(selected: Option<usize>, len: usize, delta: isize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let current = selected.unwrap_or(0) as isize;
    Some((current + delta).clamp(0, len as isize - 1) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddb::{Index, Key};
    use ratatui::crossterm::event::KeyEvent;

    fn app(rt: &tokio::runtime::Runtime) -> App {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(Vec::new(), rt.handle().clone(), tx)
    }

    /// An app showing table `t`, with a client that is never used for network calls here.
    fn table_app(rt: &tokio::runtime::Runtime, rows: Vec<Item>) -> App {
        let mut app = app(rt);
        let (client, _) = rt.block_on(ddb::connect(
            &Connection::Local("http://127.0.0.1:9".into()),
            None,
        ));
        app.client = Some(client);
        app.table = Some(table());
        app.request = Some(ReadRequest::scan("t"));
        app.screen = Screen::Items;
        app.pager.push(page(rows, None));
        app.select_first_item();
        app
    }

    fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::from(code));
    }

    fn key(name: &str, ty: &str) -> Key {
        Key {
            name: name.into(),
            ty: ty.into(),
        }
    }

    fn table() -> TableInfo {
        TableInfo {
            name: "t".into(),
            indexes: vec![
                Index {
                    name: None,
                    pk: key("pk", "S"),
                    sk: Some(key("sk", "N")),
                    global: false,
                },
                Index {
                    name: Some("by_type".into()),
                    pk: key("type", "S"),
                    sk: Some(key("sk", "N")),
                    global: true,
                },
            ],
            item_count: None,
        }
    }

    fn item(pairs: &[(&str, &str)]) -> Item {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), AttributeValue::S(v.to_string())))
            .collect()
    }

    fn page(items: Vec<Item>, next: Option<&str>) -> Page {
        Page {
            items,
            next: next.map(|t| Cursor::Token(t.into())),
            consumed: None,
            calls: Vec::new(),
        }
    }

    #[test]
    fn key_columns_come_first() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut app = app(&rt);
        app.table = Some(table());
        app.request = Some(ReadRequest::Query {
            table: "t".into(),
            index: Some("by_type".into()),
            pk: ("type".into(), AttributeValue::S("a".into())),
            sk: None,
            opts: Default::default(),
        });
        app.pager.push(page(
            vec![item(&[
                ("zeta", "1"),
                ("pk", "a"),
                ("sk", "1"),
                ("alpha", "2"),
                ("type", "x"),
            ])],
            None,
        ));
        let (cols, keys) = app.columns();
        assert_eq!(cols, ["pk", "sk", "type", "alpha", "zeta"]);
        assert_eq!(keys, 3);
    }

    #[test]
    fn keys_missing_from_every_item_are_skipped() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut app = app(&rt);
        app.table = Some(table());
        app.request = Some(ReadRequest::scan("t"));
        app.pager
            .push(page(vec![item(&[("name", "a"), ("pk", "1")])], None));
        assert_eq!(
            app.columns(),
            (vec!["pk".to_string(), "name".to_string()], 1)
        );
    }

    #[test]
    fn editing_writes_in_place_unless_the_key_changes() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut app = app(&rt);
        app.table = Some(table());
        app.request = Some(ReadRequest::scan("t"));
        let original = item(&[("pk", "a"), ("note", "x")]);
        let save_flags = |app: &mut App| match app.confirm.take() {
            Some(Confirm::Save {
                create,
                key_changed,
                ..
            }) => (create, key_changed),
            _ => panic!("expected a save confirmation"),
        };

        app.on_edited(
            original.clone(),
            Ok(Some(item(&[("pk", "a"), ("note", "y")]))),
        );
        assert_eq!(save_flags(&mut app), (false, false));

        app.on_edited(
            original.clone(),
            Ok(Some(item(&[("pk", "b"), ("note", "x")]))),
        );
        assert_eq!(
            save_flags(&mut app),
            (true, true),
            "a changed key must not replace another item"
        );
    }

    #[test]
    fn copies_and_new_items_never_replace_existing_ones() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut app = app(&rt);
        app.table = Some(table());
        app.request = Some(ReadRequest::scan("t"));
        let original = item(&[("pk", "a"), ("note", "x")]);

        app.editing = Some(EditKind::Duplicate);
        app.on_edited(original.clone(), Ok(None));
        assert!(app.confirm.is_none());
        assert!(
            matches!(&app.status, Status::Info(s) if s.contains("change the key")),
            "unchanged copy explains itself"
        );

        app.editing = Some(EditKind::Duplicate);
        app.on_edited(original, Ok(Some(item(&[("pk", "a"), ("note", "y")]))));
        assert!(matches!(
            app.confirm,
            Some(Confirm::Save {
                create: true,
                key_changed: false,
                ..
            })
        ));
    }

    #[test]
    fn create_starts_from_a_key_skeleton() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut app = table_app(&rt, vec![]);
        press(&mut app, KeyCode::Char('a'));
        let skeleton = app.take_edit_request().expect("editor request");
        assert_eq!(skeleton["pk"], AttributeValue::S(String::new()));
        assert_eq!(skeleton["sk"], AttributeValue::N("0".into()));
        assert_eq!(app.editing, Some(EditKind::Create));
    }

    #[test]
    fn delete_targets_marked_rows_by_key() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let rows: Vec<Item> = (0..3)
            .map(|i| {
                let mut it = item(&[("pk", "u"), ("note", "x")]);
                it.insert("sk".into(), AttributeValue::N(i.to_string()));
                it
            })
            .collect();
        let mut app = table_app(&rt, rows);
        press(&mut app, KeyCode::Char(' ')); // marks row 0, moves to row 1
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char(' ')); // marks row 2
        assert_eq!(app.marked, BTreeSet::from([0, 2]));

        press(&mut app, KeyCode::Char('d'));
        let Some(Confirm::Delete { table, keys }) = &app.confirm else {
            panic!("expected a delete confirmation")
        };
        assert_eq!(table, "t");
        let sks: Vec<_> = keys.iter().map(|k| k["sk"].clone()).collect();
        assert_eq!(
            sks,
            [AttributeValue::N("0".into()), AttributeValue::N("2".into())]
        );
        assert!(
            keys.iter().all(|k| k.len() == 2),
            "only key attributes are sent"
        );

        press(&mut app, KeyCode::Char('n'));
        assert!(app.confirm.is_none());
    }

    #[test]
    fn marks_clear_when_the_page_changes() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut app = table_app(&rt, vec![item(&[("pk", "a")])]);
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.marked.len(), 1);
        let g = app.generation;
        app.on_msg(Msg::Page {
            generation: g,
            replace: true,
            result: Ok(page(vec![item(&[("pk", "b")])], None)),
        });
        assert!(app.marked.is_empty());
    }

    #[test]
    fn export_prompt_writes_the_page() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut app = table_app(&rt, vec![item(&[("pk", "a"), ("note", "x")])]);
        press(&mut app, KeyCode::Char('E'));
        let prompt = app.prompt.as_mut().expect("export prompt");
        assert!(prompt.text.starts_with("t-") && prompt.text.ends_with(".csv"));

        let path =
            std::env::temp_dir().join(format!("ddb-tui-app-export-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        prompt.text = path.display().to_string();
        press(&mut app, KeyCode::Enter);
        assert!(app.prompt.is_none());
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("\"note\""), "{written}");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn going_back_drops_an_in_flight_next_page() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut app = app(&rt);
        app.screen = Screen::Items;
        app.request = Some(ReadRequest::scan("t"));
        app.pager.push(page(vec![item(&[("pk", "1")])], Some("a")));
        app.pager.push(page(vec![item(&[("pk", "2")])], Some("b")));
        let in_flight = app.generation;

        press(&mut app, KeyCode::Char('p'));
        app.on_msg(Msg::Page {
            generation: in_flight,
            replace: false,
            result: Ok(page(vec![item(&[("pk", "3")])], None)),
        });

        assert_eq!(app.pager.number(), 1);
        assert!(app.pager.next_cached(), "page 2 must still be cached");
        assert_eq!(app.items()[0]["pk"], AttributeValue::S("2".into()));
    }

    #[test]
    fn multi_line_paste_becomes_one_line_and_does_not_run() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut app = app(&rt);
        app.screen = Screen::PartiQL;
        app.on_paste("SELECT *\r\nFROM \"t\"\nWHERE pk = 'a'");
        assert_eq!(app.partiql, "SELECT * FROM \"t\" WHERE pk = 'a'");
        assert_eq!(app.screen, Screen::PartiQL, "nothing was executed");
        assert!(app.request.is_none());
    }

    #[test]
    fn page_size_steps_and_restarts_the_read() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut app = table_app(&rt, vec![item(&[("pk", "a")])]);
        press(&mut app, KeyCode::Char('+'));
        assert_eq!(app.page_size, 100);
        assert!(app.pager.is_empty(), "cached pages had the old size");
        for _ in 0..5 {
            press(&mut app, KeyCode::Char('-'));
        }
        assert_eq!(app.page_size, PAGE_SIZES[0], "stops at the smallest size");
    }

    #[test]
    fn horizontal_scroll_covers_only_non_key_columns() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut row = item(&[("pk", "a"), ("x", "1"), ("y", "2")]);
        row.insert("sk".into(), AttributeValue::N("1".into()));
        let mut app = table_app(&rt, vec![row]);
        for _ in 0..5 {
            press(&mut app, KeyCode::Right);
        }
        assert_eq!(app.col_offset, 1, "two non-key columns allow one step");
    }

    #[test]
    fn slash_starts_a_new_table_filter() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut app = app(&rt);
        app.screen = Screen::Tables;
        app.tables = vec!["alpha".into(), "beta".into()];
        app.filter = "alp".into();
        press(&mut app, KeyCode::Char('/'));
        for c in "be".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        assert_eq!(app.visible_tables(), ["beta"]);
    }

    #[test]
    fn reopening_a_table_uses_the_cached_key_schema() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut app = table_app(&rt, vec![]);
        app.table_cache.insert("t".into(), table());
        app.busy = 0;
        app.open_table("t".into());
        assert!(app.table.is_some(), "known at once");
        assert_eq!(
            app.busy, 1,
            "only the scan page is requested, no DescribeTable"
        );
    }

    #[test]
    fn tab_completes_attribute_names_in_the_filter() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut app = table_app(&rt, vec![]);
        app.seen_attrs
            .insert("t".into(), ["amount", "name"].map(String::from).into());
        app.form.query = false; // scan: focus starts on the filter
        app.screen = Screen::Form;
        for c in "am".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.form.filter, "amount");
        assert_eq!(
            app.form.active(app.table.as_ref().unwrap()),
            Field::Filter,
            "stayed"
        );
        press(&mut app, KeyCode::Tab);
        assert_eq!(
            app.form.active(app.table.as_ref().unwrap()),
            Field::Projection,
            "nothing left to complete"
        );
    }

    #[test]
    fn update_sends_only_the_key_and_the_rewritten_expression() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut row = item(&[("pk", "u"), ("note", "x")]);
        row.insert("sk".into(), AttributeValue::N("1".into()));
        let mut app = table_app(&rt, vec![row]);
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(app.screen, Screen::Update);
        for c in "SET views = views + 1".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Down);
        for c in "attribute_exists(pk)".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        let Some(Confirm::Update(req)) = &app.confirm else {
            panic!("expected an update confirmation")
        };
        assert_eq!(req.update, "SET #n0 = #n0 + :v0");
        assert_eq!(req.condition.as_deref(), Some("attribute_exists(#n1)"));
        assert_eq!(req.key.len(), 2, "pk and sk only");
        assert_eq!(req.values[":v0"], AttributeValue::N("1".into()));
    }
}
