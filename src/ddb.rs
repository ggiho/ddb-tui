//! DynamoDB calls used by the UI. Everything here is async and free of terminal state.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, anyhow};
use aws_config::BehaviorVersion;
use aws_config::meta::region::RegionProviderChain;
use aws_config::profile::{ProfileFileCredentialsProvider, ProfileFileRegionProvider};
use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::config::{Credentials, Region};
use aws_sdk_dynamodb::error::{DisplayErrorContext, ProvideErrorMetadata, SdkError};
use aws_sdk_dynamodb::types::{
    AttributeValue, ConsumedCapacity, KeySchemaElement, KeyType, ReturnConsumedCapacity,
    ReturnValue,
};

use serde_json::{Value, json};

use crate::item::{self, Item};

pub const DEFAULT_REGION: &str = "us-east-1";

#[derive(Clone, Debug, PartialEq)]
pub enum Connection {
    Profile(String),
    Local(String),
}

impl Connection {
    pub fn label(&self) -> String {
        match self {
            Self::Profile(p) => p.clone(),
            Self::Local(endpoint) => format!("local  {endpoint}"),
        }
    }
}

/// `Code: message` for service errors; the full context chain for network and credential errors.
pub(crate) fn sdk_err<E, R>(e: SdkError<E, R>) -> anyhow::Error
where
    E: ProvideErrorMetadata + std::error::Error + Send + Sync + 'static,
    R: std::fmt::Debug + Send + Sync + 'static,
{
    match (e.code(), e.message()) {
        (Some(code), Some(message)) => anyhow!("{code}: {message}"),
        _ => anyhow!("{}", DisplayErrorContext(e)),
    }
}

/// Commercial regions offered by the region picker.
pub const REGIONS: &[&str] = &[
    "us-east-1",
    "us-east-2",
    "us-west-1",
    "us-west-2",
    "ca-central-1",
    "ca-west-1",
    "mx-central-1",
    "sa-east-1",
    "eu-west-1",
    "eu-west-2",
    "eu-west-3",
    "eu-central-1",
    "eu-central-2",
    "eu-north-1",
    "eu-south-1",
    "eu-south-2",
    "il-central-1",
    "me-south-1",
    "me-central-1",
    "af-south-1",
    "ap-east-1",
    "ap-east-2",
    "ap-south-1",
    "ap-south-2",
    "ap-northeast-1",
    "ap-northeast-2",
    "ap-northeast-3",
    "ap-southeast-1",
    "ap-southeast-2",
    "ap-southeast-3",
    "ap-southeast-4",
    "ap-southeast-5",
    "ap-southeast-7",
];

/// Builds a client. Profiles use the SDK's profile provider (so SSO, assume-role and
/// credential_process work) and ignore credential environment variables. `region` overrides
/// the profile's region.
pub async fn connect(conn: &Connection, region: Option<&str>) -> (Client, String) {
    match conn {
        Connection::Profile(name) => {
            let chain = match region {
                Some(r) => RegionProviderChain::first_try(Region::new(r.to_string())),
                None => RegionProviderChain::first_try(
                    ProfileFileRegionProvider::builder()
                        .profile_name(name)
                        .build(),
                )
                .or_else(Region::new(DEFAULT_REGION)),
            };
            let conf = aws_config::defaults(BehaviorVersion::latest())
                .profile_name(name)
                .region(chain)
                .credentials_provider(
                    ProfileFileCredentialsProvider::builder()
                        .profile_name(name)
                        .build(),
                )
                .load()
                .await;
            let region = conf.region().map(|r| r.to_string()).unwrap_or_default();
            (Client::new(&conf), region)
        }
        Connection::Local(endpoint) => {
            let conf = aws_sdk_dynamodb::Config::builder()
                .behavior_version(BehaviorVersion::latest())
                .endpoint_url(endpoint)
                .region(Region::new(DEFAULT_REGION))
                .credentials_provider(Credentials::new("local", "local", None, None, "ddb-tui"))
                .build();
            (Client::from_conf(conf), DEFAULT_REGION.to_string())
        }
    }
}

pub async fn list_tables(client: &Client) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let mut start = None;
    loop {
        let out = client
            .list_tables()
            .set_exclusive_start_table_name(start)
            .send()
            .await
            .map_err(sdk_err)?;
        names.extend(out.table_names().iter().cloned());
        start = out.last_evaluated_table_name().map(str::to_string);
        if start.is_none() {
            return Ok(names);
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    pub name: String,
    pub ty: String,
}

/// Key schema of the table (`name: None`) or of one of its secondary indexes.
#[derive(Clone, Debug, PartialEq)]
pub struct Index {
    pub name: Option<String>,
    pub pk: Key,
    pub sk: Option<Key>,
    /// A global secondary index (as opposed to the table itself or an LSI).
    pub global: bool,
}

#[derive(Clone, Debug)]
pub struct TableInfo {
    pub name: String,
    /// The table's own key schema first, then GSIs and LSIs.
    pub indexes: Vec<Index>,
    pub item_count: Option<i64>,
    /// Table size in bytes, refreshed by DynamoDB about every six hours.
    pub size_bytes: Option<i64>,
}

impl TableInfo {
    pub fn base(&self) -> &Index {
        &self.indexes[0]
    }
}

impl Index {
    pub fn keys(&self) -> impl Iterator<Item = &Key> {
        std::iter::once(&self.pk).chain(self.sk.as_ref())
    }
}

/// The key attributes of `item` under `index`, or `None` if any is missing (for example
/// when a projection left them out).
pub fn key_of(item: &Item, index: &Index) -> Option<Item> {
    index
        .keys()
        .map(|k| Some((k.name.clone(), item.get(&k.name)?.clone())))
        .collect()
}

pub async fn describe(client: &Client, table: &str) -> Result<TableInfo> {
    let out = client
        .describe_table()
        .table_name(table)
        .send()
        .await
        .map_err(sdk_err)?;
    let t = out.table().context("DescribeTable returned no table")?;
    let types: HashMap<&str, &str> = t
        .attribute_definitions()
        .iter()
        .map(|d| (d.attribute_name(), d.attribute_type().as_str()))
        .collect();
    let index = |name: Option<&str>, schema: &[KeySchemaElement], global: bool| -> Result<Index> {
        let key = |kt: KeyType| {
            schema.iter().find(|k| *k.key_type() == kt).map(|k| Key {
                name: k.attribute_name().to_string(),
                ty: types
                    .get(k.attribute_name())
                    .copied()
                    .unwrap_or("S")
                    .to_string(),
            })
        };
        Ok(Index {
            name: name.map(str::to_string),
            pk: key(KeyType::Hash).context("key schema has no HASH key")?,
            sk: key(KeyType::Range),
            global,
        })
    };
    let mut indexes = vec![index(None, t.key_schema(), false)?];
    for g in t.global_secondary_indexes() {
        indexes.push(index(g.index_name(), g.key_schema(), true)?);
    }
    for l in t.local_secondary_indexes() {
        indexes.push(index(l.index_name(), l.key_schema(), false)?);
    }
    Ok(TableInfo {
        name: table.to_string(),
        indexes,
        item_count: t.item_count(),
        size_bytes: t.table_size_bytes(),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkOp {
    Eq,
    Lt,
    Le,
    Gt,
    Ge,
    BeginsWith,
    Between,
}

impl SkOp {
    pub const ALL: [SkOp; 7] = [
        Self::Eq,
        Self::Lt,
        Self::Le,
        Self::Gt,
        Self::Ge,
        Self::BeginsWith,
        Self::Between,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::BeginsWith => "begins_with",
            Self::Between => "between",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SkCond {
    pub op: SkOp,
    pub value: AttributeValue,
    /// Upper bound for `Between`.
    pub value2: Option<AttributeValue>,
}

/// Optional parts of a Scan or Query, with expressions already in placeholder form.
#[derive(Clone, Debug, Default)]
pub struct ReadOptions {
    pub filter: Option<String>,
    pub projection: Option<String>,
    pub names: HashMap<String, String>,
    pub values: HashMap<String, AttributeValue>,
    pub consistent: bool,
    /// Query only: highest sort key first.
    pub descending: bool,
    /// The options as the user typed them, for titles.
    pub summary: String,
}

#[derive(Clone, Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "one request is alive at a time, so boxing the Query fields saves nothing"
)]
pub enum ReadRequest {
    Scan {
        table: String,
        index: Option<String>,
        opts: ReadOptions,
    },
    Query {
        table: String,
        index: Option<String>,
        pk: (String, AttributeValue),
        sk: Option<(String, SkCond)>,
        opts: ReadOptions,
    },
    PartiQL {
        statement: String,
    },
}

impl ReadRequest {
    /// A plain scan of the whole table.
    pub fn scan(table: impl Into<String>) -> Self {
        Self::Scan {
            table: table.into(),
            index: None,
            opts: ReadOptions::default(),
        }
    }

    pub fn table(&self) -> Option<&str> {
        match self {
            Self::Scan { table, .. } | Self::Query { table, .. } => Some(table),
            Self::PartiQL { .. } => None,
        }
    }

    pub fn index(&self) -> Option<&str> {
        match self {
            Self::Scan { index, .. } | Self::Query { index, .. } => index.as_deref(),
            Self::PartiQL { .. } => None,
        }
    }

    pub fn options(&self) -> Option<&ReadOptions> {
        match self {
            Self::Scan { opts, .. } | Self::Query { opts, .. } => Some(opts),
            Self::PartiQL { .. } => None,
        }
    }

    /// A PartiQL statement that changes data: anything but SELECT.
    pub fn is_write(&self) -> bool {
        matches!(self, Self::PartiQL { statement } if !is_select(statement))
    }
}

/// Whether a PartiQL statement only reads. Everything else is treated as a write.
pub fn is_select(statement: &str) -> bool {
    statement
        .trim_start()
        .get(..6)
        .is_some_and(|s| s.eq_ignore_ascii_case("select"))
}

fn non_empty<K, V>(map: HashMap<K, V>) -> Option<HashMap<K, V>> {
    (!map.is_empty()).then_some(map)
}

/// Where the next page starts: a key for Scan/Query, a token for PartiQL.
#[derive(Clone, Debug, PartialEq)]
pub enum Cursor {
    Key(Item),
    Token(String),
}

pub struct Page {
    pub items: Vec<Item>,
    pub next: Option<Cursor>,
    /// Read capacity consumed by the calls that built this page; `None` when the service
    /// reported none (DynamoDB Local omits it for some operations).
    pub consumed: Option<f64>,
    /// Request parameters and response metadata of each call, for inspection.
    pub calls: Vec<Value>,
}

/// Fetches up to `limit` items from `start`, following continuations until the page is full
/// or the read is exhausted; filters and the 1 MB response cap otherwise return short pages.
pub async fn fetch_page(
    client: &Client,
    req: &ReadRequest,
    start: Option<Cursor>,
    limit: usize,
) -> Result<Page> {
    let mut page = Page {
        items: Vec::new(),
        next: start,
        consumed: None,
        calls: Vec::new(),
    };
    loop {
        let want = (limit - page.items.len()) as i32;
        let call = fetch_once(client, req, page.next.take(), want).await?;
        page.items.extend(call.items);
        page.next = call.next;
        page.consumed = add(page.consumed, call.consumed);
        page.calls.push(call.log);
        if page.next.is_none() || page.items.len() >= limit {
            return Ok(page);
        }
    }
}

/// One Scan, Query or ExecuteStatement call, without filling up to a page: long reads use it
/// to report progress and notice a cancel after every request, however selective the filter.
pub async fn fetch_call(
    client: &Client,
    req: &ReadRequest,
    start: Option<Cursor>,
    limit: usize,
) -> Result<Page> {
    let call = fetch_once(client, req, start, limit as i32).await?;
    Ok(Page {
        items: call.items,
        next: call.next,
        consumed: call.consumed,
        calls: vec![call.log],
    })
}

struct Call {
    items: Vec<Item>,
    next: Option<Cursor>,
    consumed: Option<f64>,
    log: Value,
}

fn units(c: Option<&ConsumedCapacity>) -> Option<f64> {
    c.and_then(|c| c.capacity_units())
}

/// Sums capacity, staying `None` only while nothing has been reported.
fn add(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (None, None) => None,
        _ => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
    }
}

fn names_json(names: &HashMap<String, String>) -> Value {
    if names.is_empty() {
        Value::Null
    } else {
        json!(names)
    }
}

fn values_json(values: &HashMap<String, AttributeValue>) -> Value {
    if values.is_empty() {
        Value::Null
    } else {
        item::item_to_json(values)
    }
}

/// `{"request": …, "response": …}` with null fields dropped.
fn call_log(request: Value, response: Value) -> Value {
    let strip = |v: Value| match v {
        Value::Object(m) => Value::Object(m.into_iter().filter(|(_, v)| !v.is_null()).collect()),
        other => other,
    };
    json!({ "request": strip(request), "response": strip(response) })
}

async fn fetch_once(
    client: &Client,
    req: &ReadRequest,
    cursor: Option<Cursor>,
    limit: i32,
) -> Result<Call> {
    let start_key = match &cursor {
        Some(Cursor::Key(k)) => Some(k.clone()),
        _ => None,
    };
    let start_json = start_key.as_ref().map(item::item_to_json);
    match req {
        ReadRequest::Scan { table, index, opts } => {
            let out = client
                .scan()
                .table_name(table)
                .set_index_name(index.clone())
                .limit(limit)
                .set_exclusive_start_key(start_key)
                .set_filter_expression(opts.filter.clone())
                .set_projection_expression(opts.projection.clone())
                .set_expression_attribute_names(non_empty(opts.names.clone()))
                .set_expression_attribute_values(non_empty(opts.values.clone()))
                .consistent_read(opts.consistent)
                .return_consumed_capacity(ReturnConsumedCapacity::Total)
                .send()
                .await
                .map_err(sdk_err)?;
            let next = out.last_evaluated_key().cloned().map(Cursor::Key);
            let consumed = units(out.consumed_capacity());
            let log = call_log(
                json!({
                    "Operation": "Scan",
                    "TableName": table,
                    "IndexName": index,
                    "Limit": limit,
                    "ExclusiveStartKey": start_json,
                    "FilterExpression": opts.filter,
                    "ProjectionExpression": opts.projection,
                    "ExpressionAttributeNames": names_json(&opts.names),
                    "ExpressionAttributeValues": values_json(&opts.values),
                    "ConsistentRead": opts.consistent,
                }),
                json!({
                    "Count": out.count(),
                    "ScannedCount": out.scanned_count(),
                    "LastEvaluatedKey": out.last_evaluated_key().map(item::item_to_json),
                    "ConsumedCapacityUnits": consumed,
                }),
            );
            Ok(Call {
                items: out.items().to_vec(),
                next,
                consumed,
                log,
            })
        }
        ReadRequest::Query {
            table,
            index,
            pk,
            sk,
            opts,
        } => {
            let (expr, mut names, mut values) = key_condition(pk, sk.as_ref());
            names.extend(opts.names.clone());
            values.extend(opts.values.clone());
            let request = json!({
                "Operation": "Query",
                "TableName": table,
                "IndexName": index,
                "KeyConditionExpression": expr,
                "FilterExpression": opts.filter,
                "ProjectionExpression": opts.projection,
                "ExpressionAttributeNames": names_json(&names),
                "ExpressionAttributeValues": values_json(&values),
                "ConsistentRead": opts.consistent,
                "ScanIndexForward": !opts.descending,
                "Limit": limit,
                "ExclusiveStartKey": start_json,
            });
            let out = client
                .query()
                .table_name(table)
                .set_index_name(index.clone())
                .key_condition_expression(expr)
                .set_filter_expression(opts.filter.clone())
                .set_projection_expression(opts.projection.clone())
                .set_expression_attribute_names(Some(names))
                .set_expression_attribute_values(Some(values))
                .consistent_read(opts.consistent)
                .scan_index_forward(!opts.descending)
                .limit(limit)
                .set_exclusive_start_key(start_key)
                .return_consumed_capacity(ReturnConsumedCapacity::Total)
                .send()
                .await
                .map_err(sdk_err)?;
            let next = out.last_evaluated_key().cloned().map(Cursor::Key);
            let consumed = units(out.consumed_capacity());
            let log = call_log(
                request,
                json!({
                    "Count": out.count(),
                    "ScannedCount": out.scanned_count(),
                    "LastEvaluatedKey": out.last_evaluated_key().map(item::item_to_json),
                    "ConsumedCapacityUnits": consumed,
                }),
            );
            Ok(Call {
                items: out.items().to_vec(),
                next,
                consumed,
                log,
            })
        }
        ReadRequest::PartiQL { statement } => {
            let token = match cursor {
                Some(Cursor::Token(t)) => Some(t),
                _ => None,
            };
            // Limit only applies to SELECT; writes run once and return no items.
            let limit = is_select(statement).then_some(limit);
            let out = client
                .execute_statement()
                .statement(statement)
                .set_limit(limit)
                .set_next_token(token.clone())
                .return_consumed_capacity(ReturnConsumedCapacity::Total)
                .send()
                .await
                .map_err(sdk_err)?;
            let next = out.next_token().map(|t| Cursor::Token(t.to_string()));
            let consumed = units(out.consumed_capacity());
            let log = call_log(
                json!({ "Operation": "ExecuteStatement", "Statement": statement, "Limit": limit, "NextToken": token }),
                json!({
                    "Count": out.items().len(),
                    "NextToken": out.next_token(),
                    "ConsumedCapacityUnits": consumed,
                }),
            );
            Ok(Call {
                items: out.items().to_vec(),
                next,
                consumed,
                log,
            })
        }
    }
}

type Condition = (
    String,
    HashMap<String, String>,
    HashMap<String, AttributeValue>,
);

fn key_condition(pk: &(String, AttributeValue), sk: Option<&(String, SkCond)>) -> Condition {
    let mut names = HashMap::from([("#pk".to_string(), pk.0.clone())]);
    let mut values = HashMap::from([(":pk".to_string(), pk.1.clone())]);
    let mut expr = "#pk = :pk".to_string();
    if let Some((name, c)) = sk {
        names.insert("#sk".into(), name.clone());
        values.insert(":sk".into(), c.value.clone());
        let cond = match c.op {
            SkOp::BeginsWith => "begins_with(#sk, :sk)".to_string(),
            SkOp::Between => {
                values.insert(
                    ":sk2".into(),
                    c.value2.clone().unwrap_or_else(|| c.value.clone()),
                );
                "#sk BETWEEN :sk AND :sk2".to_string()
            }
            op => format!("#sk {} :sk", op.label()),
        };
        expr = format!("{expr} AND {cond}");
    }
    (expr, names, values)
}

/// Strongly consistent read of one item by its full key.
pub async fn get_item(client: &Client, table: &str, key: Item) -> Result<Option<Item>> {
    let out = client
        .get_item()
        .table_name(table)
        .set_key(Some(key))
        .consistent_read(true)
        .send()
        .await
        .map_err(sdk_err)?;
    Ok(out.item().cloned())
}

/// Writes the item and returns the write capacity used. With `only_if_absent` (the table's
/// partition key name) the write fails instead of replacing an existing item.
pub async fn put_item(
    client: &Client,
    table: &str,
    item: Item,
    only_if_absent: Option<&str>,
) -> Result<Option<f64>> {
    let mut put = client
        .put_item()
        .table_name(table)
        .set_item(Some(item))
        .return_consumed_capacity(ReturnConsumedCapacity::Total);
    if let Some(pk) = only_if_absent {
        put = put
            .condition_expression("attribute_not_exists(#pk)")
            .expression_attribute_names("#pk", pk);
    }
    match put.send().await {
        Ok(out) => Ok(units(out.consumed_capacity())),
        Err(e) if e.code() == Some("ConditionalCheckFailedException") => Err(anyhow!(
            "An item with this key already exists; change the key"
        )),
        Err(e) => Err(sdk_err(e)),
    }
}

/// An UpdateItem with expressions already in placeholder form.
#[derive(Clone, Debug)]
pub struct UpdateRequest {
    pub table: String,
    pub key: Item,
    pub update: String,
    pub condition: Option<String>,
    pub names: HashMap<String, String>,
    pub values: HashMap<String, AttributeValue>,
    /// The expressions as the user typed them, for the confirmation.
    pub summary: String,
}

/// Runs the update and returns the item as written plus the write capacity used.
pub async fn update_item(
    client: &Client,
    req: UpdateRequest,
) -> Result<(Option<Item>, Option<f64>)> {
    let out = client
        .update_item()
        .table_name(req.table)
        .set_key(Some(req.key))
        .update_expression(req.update)
        .set_condition_expression(req.condition)
        .set_expression_attribute_names(non_empty(req.names))
        .set_expression_attribute_values(non_empty(req.values))
        .return_values(ReturnValue::AllNew)
        .return_consumed_capacity(ReturnConsumedCapacity::Total)
        .send()
        .await;
    match out {
        Ok(out) => Ok((out.attributes().cloned(), units(out.consumed_capacity()))),
        Err(e) if e.code() == Some("ConditionalCheckFailedException") => Err(anyhow!(
            "The condition was not met, or the item no longer exists; nothing was changed"
        )),
        Err(e) => Err(sdk_err(e)),
    }
}

pub struct DeleteReport {
    pub requested: usize,
    pub deleted: usize,
    /// `key: error` for each item that could not be deleted.
    pub failed: Vec<String>,
    pub consumed: Option<f64>,
}

/// Deletes items one by one, like Workbench, collecting failures instead of stopping.
pub async fn delete_items(client: &Client, table: &str, keys: Vec<Item>) -> DeleteReport {
    let mut report = DeleteReport {
        requested: keys.len(),
        deleted: 0,
        failed: Vec::new(),
        consumed: None,
    };
    for key in keys {
        let label = item::item_to_plain_json(&key).to_string();
        match client
            .delete_item()
            .table_name(table)
            .set_key(Some(key))
            .return_consumed_capacity(ReturnConsumedCapacity::Total)
            .send()
            .await
        {
            Ok(out) => {
                report.deleted += 1;
                report.consumed = add(report.consumed, units(out.consumed_capacity()));
            }
            Err(e) => report.failed.push(format!("{label}: {:#}", sdk_err(e))),
        }
    }
    report
}

/// Fresh copies of `keys`, in order; `None` for an item that no longer exists.
pub async fn get_items(
    client: &Client,
    table: &str,
    keys: Vec<Item>,
) -> Vec<(Item, Result<Option<Item>>)> {
    let mut items = Vec::new();
    for key in keys {
        let item = get_item(client, table, key.clone()).await;
        items.push((key, item));
    }
    items
}

/// How far an UpdateItem applied to several items has come.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UpdateProgress {
    pub total: usize,
    /// Updated or failed so far.
    pub done: usize,
    pub failed: usize,
    pub consumed: f64,
}

/// How an UpdateItem applied to several items went.
#[derive(Debug, Default)]
pub struct UpdateReport {
    pub requested: usize,
    pub updated: usize,
    /// Each item that was not updated, with why, such as an unmet condition.
    pub failed: Vec<(Item, String)>,
    /// Items not sent because the update was cancelled.
    pub skipped: Vec<Item>,
    pub consumed: Option<f64>,
}

/// Requests in flight at once while updating many items.
pub const UPDATE_CONCURRENCY: usize = 8;

/// Applies `req` to each key, a few at a time. A failure, such as an unmet condition, does not
/// stop the others; once `cancel` is set no more are sent and the rest are reported skipped.
pub async fn update_items(
    client: &Client,
    req: &UpdateRequest,
    keys: Vec<Item>,
    cancel: &AtomicBool,
    progress: impl Fn(UpdateProgress),
) -> UpdateReport {
    let mut report = UpdateReport {
        requested: keys.len(),
        ..UpdateReport::default()
    };
    let mut status = UpdateProgress {
        total: keys.len(),
        ..UpdateProgress::default()
    };
    let mut pending = keys.into_iter();
    let mut running = tokio::task::JoinSet::new();
    loop {
        while running.len() < UPDATE_CONCURRENCY && !cancel.load(Ordering::Relaxed) {
            let Some(key) = pending.next() else { break };
            let client = client.clone();
            let one = UpdateRequest {
                key: key.clone(),
                ..req.clone()
            };
            running.spawn(async move { (key, update_item(&client, one).await) });
        }
        let Some(joined) = running.join_next().await else {
            break;
        };
        match joined {
            Ok((_, Ok((_, units)))) => {
                report.updated += 1;
                report.consumed = add(report.consumed, units);
            }
            Ok((key, Err(e))) => report.failed.push((key, format!("{e:#}"))),
            Err(e) => report.failed.push((Item::new(), format!("{e:#}"))),
        }
        status.done = report.updated + report.failed.len();
        status.failed = report.failed.len();
        status.consumed = report.consumed.unwrap_or(0.0);
        progress(status);
    }
    report.skipped = pending.collect();
    report
}

/// The keys of every item `req` reads, to apply one update to all of them. Only the key
/// attributes are fetched. `None` when cancelled.
pub async fn collect_keys(
    client: &Client,
    req: &ReadRequest,
    base: &Index,
    cancel: &AtomicBool,
    progress: impl Fn(usize, f64),
) -> Result<Option<Vec<Item>>> {
    let req = with_key_projection(req, base);
    let mut keys = Vec::new();
    let mut consumed = 0.0;
    let mut cursor = None;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let page = fetch_call(client, &req, cursor, 1000).await?;
        consumed += page.consumed.unwrap_or(0.0);
        keys.extend(page.items.iter().filter_map(|i| key_of(i, base)));
        progress(keys.len(), consumed);
        cursor = page.next;
        if cursor.is_none() {
            return Ok(Some(keys));
        }
    }
}

/// `req` reading only the table's key attributes. Names the old projection alone used are
/// dropped, since DynamoDB rejects unused placeholders.
fn with_key_projection(req: &ReadRequest, base: &Index) -> ReadRequest {
    let mut req = req.clone();
    if let ReadRequest::Scan { opts, .. } | ReadRequest::Query { opts, .. } = &mut req {
        let filter = opts.filter.clone().unwrap_or_default();
        opts.names
            .retain(|placeholder, _| uses(&filter, placeholder));
        let mut parts = Vec::new();
        for (i, key) in base.keys().enumerate() {
            let placeholder = format!("#ddbtuikey{i}");
            opts.names.insert(placeholder.clone(), key.name.clone());
            parts.push(placeholder);
        }
        opts.projection = Some(parts.join(", "));
    }
    req
}

/// Whether `expr` mentions `placeholder` as a whole token (`#n1` is not in `#n10`).
fn uses(expr: &str, placeholder: &str) -> bool {
    expr.match_indices(placeholder).any(|(at, _)| {
        !expr[at + placeholder.len()..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> AttributeValue {
        AttributeValue::S(v.into())
    }

    #[test]
    fn key_projection_keeps_only_the_names_the_filter_uses() {
        let base = Index {
            name: None,
            pk: Key {
                name: "pk".into(),
                ty: "S".into(),
            },
            sk: Some(Key {
                name: "sk".into(),
                ty: "N".into(),
            }),
            global: false,
        };
        let req = ReadRequest::Scan {
            table: "t".into(),
            index: None,
            opts: ReadOptions {
                filter: Some("#n10 = :v0".into()),
                projection: Some("#n1, #n10".into()),
                names: HashMap::from([
                    ("#n1".to_string(), "name".to_string()),
                    ("#n10".to_string(), "status".to_string()),
                ]),
                ..ReadOptions::default()
            },
        };
        let ReadRequest::Scan { opts, .. } = with_key_projection(&req, &base) else {
            unreachable!()
        };
        assert_eq!(opts.projection.as_deref(), Some("#ddbtuikey0, #ddbtuikey1"));
        let mut names: Vec<_> = opts.names.into_iter().collect();
        names.sort();
        assert_eq!(
            names,
            [
                ("#ddbtuikey0".to_string(), "pk".to_string()),
                ("#ddbtuikey1".to_string(), "sk".to_string()),
                ("#n10".to_string(), "status".to_string()),
            ],
            "#n1 was only in the projection"
        );
    }

    #[test]
    fn partition_key_only() {
        let (expr, names, values) = key_condition(&("id".into(), s("a")), None);
        assert_eq!(expr, "#pk = :pk");
        assert_eq!(names["#pk"], "id");
        assert_eq!(values[":pk"], s("a"));
    }

    #[test]
    fn sort_key_operators() {
        let pk = ("id".to_string(), s("a"));
        let cond = |op, value2| {
            (
                "ts".to_string(),
                SkCond {
                    op,
                    value: s("1"),
                    value2,
                },
            )
        };
        assert_eq!(
            key_condition(&pk, Some(&cond(SkOp::Ge, None))).0,
            "#pk = :pk AND #sk >= :sk"
        );
        assert_eq!(
            key_condition(&pk, Some(&cond(SkOp::BeginsWith, None))).0,
            "#pk = :pk AND begins_with(#sk, :sk)"
        );
        let (expr, names, values) = key_condition(&pk, Some(&cond(SkOp::Between, Some(s("9")))));
        assert_eq!(expr, "#pk = :pk AND #sk BETWEEN :sk AND :sk2");
        assert_eq!(names["#sk"], "ts");
        assert_eq!(values[":sk2"], s("9"));
    }
}
