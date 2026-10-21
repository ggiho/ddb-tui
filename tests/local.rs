//! End-to-end checks against DynamoDB Local. Skipped unless DDB_LOCAL_ENDPOINT is set, e.g.
//! `DDB_LOCAL_ENDPOINT=http://localhost:8765 cargo test --test local`.

use std::collections::HashSet;

use anyhow::Result;
use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::types::{
    AttributeDefinition, AttributeValue, BillingMode, GlobalSecondaryIndex, KeySchemaElement,
    KeyType, Projection, ProjectionType, ScalarAttributeType,
};
use ddb_tui::ddb::{self, Connection, Page, ReadRequest, SkCond, SkOp};
use ddb_tui::expr::Placeholders;
use ddb_tui::form::ReadForm;
use ddb_tui::item::Item;
use ddb_tui::saved;

const ITEMS: usize = 120;

fn s(v: &str) -> AttributeValue {
    AttributeValue::S(v.into())
}

fn n(v: usize) -> AttributeValue {
    AttributeValue::N(v.to_string())
}

fn attr(name: &str, ty: ScalarAttributeType) -> Result<AttributeDefinition> {
    Ok(AttributeDefinition::builder()
        .attribute_name(name)
        .attribute_type(ty)
        .build()?)
}

fn key(name: &str, ty: KeyType) -> Result<KeySchemaElement> {
    Ok(KeySchemaElement::builder()
        .attribute_name(name)
        .key_type(ty)
        .build()?)
}

/// pk = user#(i % 3), sk = i, type = odd/even, plus a nested map.
async fn create_table(client: &Client, table: &str) -> Result<()> {
    client
        .create_table()
        .table_name(table)
        .attribute_definitions(attr("pk", ScalarAttributeType::S)?)
        .attribute_definitions(attr("sk", ScalarAttributeType::N)?)
        .attribute_definitions(attr("type", ScalarAttributeType::S)?)
        .key_schema(key("pk", KeyType::Hash)?)
        .key_schema(key("sk", KeyType::Range)?)
        .global_secondary_indexes(
            GlobalSecondaryIndex::builder()
                .index_name("by_type")
                .key_schema(key("type", KeyType::Hash)?)
                .key_schema(key("sk", KeyType::Range)?)
                .projection(
                    Projection::builder()
                        .projection_type(ProjectionType::All)
                        .build(),
                )
                .build()?,
        )
        .billing_mode(BillingMode::PayPerRequest)
        .send()
        .await?;
    for i in 0..ITEMS {
        let item = Item::from([
            ("pk".to_string(), s(&format!("user#{}", i % 3))),
            ("sk".to_string(), n(i)),
            (
                "type".to_string(),
                s(if i % 2 == 1 { "odd" } else { "even" }),
            ),
            (
                "meta".to_string(),
                AttributeValue::M(Item::from([("i".to_string(), n(i))])),
            ),
        ]);
        ddb::put_item(client, table, item, None).await?;
    }
    Ok(())
}

async fn all_pages(client: &Client, req: &ReadRequest, limit: usize) -> Result<Vec<Page>> {
    let mut pages = vec![ddb::fetch_page(client, req, None, limit).await?];
    while let Some(next) = pages.last().unwrap().next.clone() {
        pages.push(ddb::fetch_page(client, req, Some(next), limit).await?);
    }
    Ok(pages)
}

fn sizes(pages: &[Page]) -> Vec<usize> {
    pages.iter().map(|p| p.items.len()).collect()
}

#[tokio::test]
async fn reads_and_writes_against_dynamodb_local() -> Result<()> {
    let Ok(endpoint) = std::env::var("DDB_LOCAL_ENDPOINT") else {
        eprintln!("skipped: set DDB_LOCAL_ENDPOINT to run against DynamoDB Local");
        return Ok(());
    };
    let (client, _) = ddb::connect(&Connection::Local(endpoint), None).await;
    let table = format!("ddb_tui_it_{}", std::process::id());
    create_table(&client, &table).await?;
    let result = checks(&client, &table).await;
    client.delete_table().table_name(&table).send().await?;
    result
}

async fn checks(client: &Client, table: &str) -> Result<()> {
    assert!(ddb::list_tables(client).await?.contains(&table.to_string()));

    let info = ddb::describe(client, table).await?;
    assert_eq!(
        (info.base().pk.name.as_str(), info.base().pk.ty.as_str()),
        ("pk", "S")
    );
    assert_eq!(info.base().sk.as_ref().map(|k| k.ty.as_str()), Some("N"));
    assert_eq!(info.indexes[1].name.as_deref(), Some("by_type"));

    // Scan: 120 items in pages of 50, every item exactly once.
    let scan = ReadRequest::scan(table);
    let pages = all_pages(client, &scan, 50).await?;
    assert_eq!(sizes(&pages), [50, 50, 20]);
    let keys: HashSet<String> = pages
        .iter()
        .flat_map(|p| &p.items)
        .map(|i| format!("{:?}{:?}", i["pk"], i["sk"]))
        .collect();
    assert_eq!(keys.len(), ITEMS);

    // Query with a sort key range: i % 3 == 1 and 10 <= i <= 50, ascending.
    let between = ReadRequest::Query {
        table: table.into(),
        index: None,
        pk: ("pk".into(), s("user#1")),
        sk: Some((
            "sk".into(),
            SkCond {
                op: SkOp::Between,
                value: n(10),
                value2: Some(n(50)),
            },
        )),
        opts: Default::default(),
    };
    let page = ddb::fetch_page(client, &between, None, 50).await?;
    let got: Vec<String> = page
        .items
        .iter()
        .map(|i| i["sk"].as_n().unwrap().clone())
        .collect();
    let want: Vec<String> = (10..=50)
        .filter(|i| i % 3 == 1)
        .map(|i| i.to_string())
        .collect();
    assert_eq!(got, want);
    assert!(page.next.is_none());

    // Query a GSI across pages.
    let odd = ReadRequest::Query {
        table: table.into(),
        index: Some("by_type".into()),
        pk: ("type".into(), s("odd")),
        sk: None,
        opts: Default::default(),
    };
    assert_eq!(sizes(&all_pages(client, &odd, 50).await?), [50, 10]);

    // PartiQL on the partition key.
    let user2 = ReadRequest::PartiQL {
        statement: format!("SELECT * FROM \"{table}\" WHERE pk = 'user#2'"),
    };
    assert_eq!(sizes(&all_pages(client, &user2, 50).await?), [40]);

    // A filtered read evaluates `limit` items per call and returns about half; the page must
    // still be filled to 50 by following the continuation token.
    let statement = format!("SELECT * FROM \"{table}\" WHERE \"type\" = 'odd'");
    let single = client
        .execute_statement()
        .statement(&statement)
        .limit(50)
        .send()
        .await?;
    assert!(
        single.items().len() < 50 && single.next_token().is_some(),
        "precondition: one call returns a short page"
    );
    let filtered = ReadRequest::PartiQL { statement };
    let pages = all_pages(client, &filtered, 50).await?;
    assert_eq!(pages[0].items.len(), 50);
    assert_eq!(pages.iter().map(|p| p.items.len()).sum::<usize>(), 60);

    // Writes round-trip.
    let mut item = page.items[0].clone();
    item.insert("note".into(), s("edited by test"));
    ddb::put_item(client, table, item.clone(), None).await?;
    let one = ReadRequest::Query {
        table: table.into(),
        index: None,
        pk: ("pk".into(), item["pk"].clone()),
        sk: Some((
            "sk".into(),
            SkCond {
                op: SkOp::Eq,
                value: item["sk"].clone(),
                value2: None,
            },
        )),
        opts: Default::default(),
    };
    assert_eq!(
        ddb::fetch_page(client, &one, None, 50).await?.items,
        [item.clone()]
    );

    form_reads(client, table, &info, &item).await?;
    writes_and_capacity(client, table, &item).await?;
    table_details(client, table).await?;
    updates_and_saved_reads(client, table, &info).await
}

/// UpdateItem expressions as typed in the update popup, and saved operations that round-trip.
async fn updates_and_saved_reads(
    client: &Client,
    table: &str,
    info: &ddb::TableInfo,
) -> Result<()> {
    let key = Item::from([("pk".to_string(), s("user#1")), ("sk".to_string(), n(10))]);
    let update = |text: &str, condition: Option<&str>| -> Result<ddb::UpdateRequest> {
        let mut ph = Placeholders::default();
        let update = ph.update(text)?;
        let condition = condition.map(|c| ph.condition(c)).transpose()?;
        Ok(ddb::UpdateRequest {
            table: table.into(),
            key: key.clone(),
            update,
            condition,
            names: ph.names,
            values: ph.values,
            summary: String::new(),
        })
    };
    let text = "SET views = if_not_exists(views, 0) + 1, hist = list_append(if_not_exists(hist, []), [\"a\"]) ADD labels <<\"x\">> REMOVE meta";
    let (item, wcu) = ddb::update_item(client, update(text, Some("attribute_exists(pk)"))?).await?;
    let item = item.expect("ALL_NEW returns the item");
    assert_eq!(item["views"], n(1));
    assert_eq!(item["hist"], AttributeValue::L(vec![s("a")]));
    assert_eq!(item["labels"], AttributeValue::Ss(vec!["x".into()]));
    assert!(!item.contains_key("meta"));
    assert!(wcu.is_some());
    let (item, _) = ddb::update_item(client, update(text, None)?).await?;
    assert_eq!(item.unwrap()["views"], n(2), "the counter increments");

    let err = ddb::update_item(
        client,
        update("SET views = views + 1", Some("views > 100"))?,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("condition was not met"), "{err}");
    let unchanged = ddb::get_item(client, table, key.clone()).await?.unwrap();
    assert_eq!(unchanged["views"], n(2));

    // A Query saved in Workbench format reads back into the same request.
    let mut form = ReadForm::default();
    form.pk = "user#1".into();
    form.op = 7; // between
    form.sk = "10".into();
    form.sk2 = "50".into();
    form.filter = "type = 'odd' OR NOT attribute_exists(views)".into();
    form.descending = true;
    let dir = std::env::temp_dir().join(format!("ddb-tui-it-saved-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = saved::save_read(&dir, "user1 range", info, &form)?;
    let saved::Loaded::Read(read) = saved::load(&path)? else {
        panic!("expected a saved read")
    };
    let loaded = read.into_form(info);
    let a = ddb::fetch_page(client, &form.request(info)?, None, 50)
        .await?
        .items;
    let b = ddb::fetch_page(client, &loaded.request(info)?, None, 50)
        .await?
        .items;
    assert!(!a.is_empty());
    assert_eq!(a, b, "saved and reloaded queries return the same items");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

async fn table_details(client: &Client, table: &str) -> Result<()> {
    let text = ddb_tui::details::report(client, table).await?;
    for expected in [
        table,
        "pk (S, HASH) · sk (N, RANGE)",
        "PAY_PER_REQUEST",
        "by_type · type (S, HASH) · sk (N, RANGE) · ALL",
        "Deletion protection",
        "TTL",
        "Point-in-time recovery",
        "Tags",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in\n{text}");
    }
    assert!(
        ddb_tui::details::report(client, "no_such_table")
            .await
            .is_err()
    );
    println!("{text}");
    Ok(())
}

/// Conditional creates, bulk deletes, consumed capacity and the per-call log.
async fn writes_and_capacity(client: &Client, table: &str, existing: &Item) -> Result<()> {
    // Creating over an existing key is refused instead of silently replacing the item.
    let err = ddb::put_item(client, table, existing.clone(), Some("pk"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("already exists"), "{err}");

    let mut fresh = existing.clone();
    fresh.insert("pk".into(), s("new#1"));
    let wcu = ddb::put_item(client, table, fresh.clone(), Some("pk")).await?;
    assert!(
        wcu.is_some_and(|w| w > 0.0),
        "write capacity reported: {wcu:?}"
    );

    // One good key and one without its sort key: the bad one is reported, the good one deleted.
    let good = Item::from([
        ("pk".to_string(), s("new#1")),
        ("sk".to_string(), fresh["sk"].clone()),
    ]);
    let bad = Item::from([("pk".to_string(), s("new#1"))]);
    let report = ddb::delete_items(client, table, vec![good.clone(), bad]).await;
    assert_eq!(
        (report.requested, report.deleted, report.failed.len()),
        (2, 1, 1)
    );
    assert!(
        report.failed[0].contains("ValidationException"),
        "{:?}",
        report.failed
    );
    assert_eq!(ddb::get_item(client, table, good).await?, None);

    // A filtered scan that needs several calls logs each of them and sums their capacity.
    let mut form = ReadForm::default();
    form.query = false;
    form.filter = "type = 'odd'".into();
    let req = form.request(&ddb::describe(client, table).await?)?;
    let page = ddb::fetch_page(client, &req, None, 50).await?;
    assert!(page.calls.len() > 1, "a filtered page takes several calls");
    assert!(
        page.consumed.is_some_and(|c| c > 0.0),
        "read capacity reported: {:?}",
        page.consumed
    );
    let first = &page.calls[0];
    assert_eq!(first["request"]["Operation"], "Scan");
    assert_eq!(first["request"]["ExpressionAttributeNames"]["#n0"], "type");
    assert_eq!(
        first["request"]["ExpressionAttributeValues"][":v0"]["S"],
        "odd"
    );
    let scanned: i64 = page
        .calls
        .iter()
        .map(|c| c["response"]["ScannedCount"].as_i64().unwrap())
        .sum();
    assert!(
        scanned > page.items.len() as i64,
        "the filter discarded scanned items"
    );
    Ok(())
}

/// Reads built through the Scan/Query form, the way the UI builds them.
async fn form_reads(
    client: &Client,
    table: &str,
    info: &ddb::TableInfo,
    edited: &Item,
) -> Result<()> {
    let read = |set: fn(&mut ReadForm)| {
        let mut form = ReadForm::default();
        set(&mut form);
        form.request(info)
    };
    let total = |pages: &[Page]| pages.iter().map(|p| p.items.len()).sum::<usize>();

    // `type` is a reserved word; the placeholder rewrite makes it usable as written.
    let odd = read(|f| {
        f.query = false;
        f.filter = "type = 'odd'".into();
    })?;
    let pages = all_pages(client, &odd, 50).await?;
    assert_eq!((pages[0].items.len(), total(&pages)), (50, 60));

    // A projection returns only the named paths.
    let projected = read(|f| {
        f.query = false;
        f.projection = "pk, meta.i".into();
    })?;
    for it in ddb::fetch_page(client, &projected, None, 5).await?.items {
        let mut names: Vec<&String> = it.keys().collect();
        names.sort();
        assert_eq!(names, ["meta", "pk"]);
        assert!(it["meta"].as_m().unwrap().contains_key("i"));
    }

    // Descending, strongly consistent query filtered on a nested attribute.
    let desc = read(|f| {
        f.pk = "user#1".into();
        f.filter = "meta.i < 20".into();
        f.consistent = true;
        f.descending = true;
    })?;
    let got: Vec<String> = ddb::fetch_page(client, &desc, None, 50)
        .await?
        .items
        .iter()
        .map(|i| i["sk"].as_n().unwrap().clone())
        .collect();
    let want: Vec<String> = (0..20)
        .rev()
        .filter(|i| i % 3 == 1)
        .map(|i| i.to_string())
        .collect();
    assert_eq!(got, want);

    // Filtered scan of a GSI.
    let gsi = read(|f| {
        f.query = false;
        f.index = 1;
        f.filter = "sk < 10".into();
    })?;
    assert_eq!(total(&all_pages(client, &gsi, 50).await?), 10);

    // GetItem returns the complete item, or None for a key that does not exist.
    let key = ddb::key_of(edited, info.base()).unwrap();
    assert_eq!(
        ddb::get_item(client, table, key).await?.as_ref(),
        Some(edited)
    );
    let missing = Item::from([("pk".to_string(), s("nobody")), ("sk".to_string(), n(0))]);
    assert_eq!(ddb::get_item(client, table, missing).await?, None);
    Ok(())
}
