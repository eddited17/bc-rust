//! GetPage on the embedded store (`boot(&root, Db::Memory)`) with bc-server's demo
//! seed, the Base App's Customer table and the Customer List page. Skipped when
//! vendor/bcapps is not present.
//!
//! The seed inserts the eight customers below through the AL triggers, so the No.
//! Series numbers them C10000 .. C10070, and writes Detailed Cust. Ledg. Entries
//! that Balance (LCY) sums.

use super::*;
use al_runtime::{Runtime, Session};
use bc_server::app::{boot, Db};
use bc_store::Value;
use rust_decimal::Decimal;
use std::sync::OnceLock;

/// Name, address, city, country of the seeded customers, in No. order.
const CUSTOMERS: &[(&str, &str, &str, &str)] = &[
    ("Adatum Corporation", "192 Market Square", "Atlanta", "US"),
    ("Trey Research", "153 Thomas Drive", "Chicago", "US"),
    ("School of Fine Art", "10 High Tower Green", "Miami", "US"),
    ("Alpine Ski House", "10 Deerfield Road", "London", "GB"),
    ("Relecloud", "25 Water Way", "Copenhagen", "DK"),
    ("Coho Winery", "1 Vineyard Lane", "Napa", "US"),
    ("Fabrikam, Inc.", "51 Industrial Park", "Berlin", "DE"),
    ("Litware, Inc.", "77 Software Street", "Seattle", "US"),
];

const CUSTOMER_LIST: i64 = 22;

fn no(i: usize) -> String {
    format!("C{}", 10000 + 10 * i)
}

/// Balance (LCY) of customer i: the seed's opening entries.
fn balance(i: usize) -> Decimal {
    (0..(i % 3 + 1))
        .map(|k| Decimal::from(((i as i64 + 3) * 1375 + k as i64 * 412) % 9000 + 250))
        .sum()
}

pub(crate) fn runtime() -> Option<&'static Runtime> {
    static RT: OnceLock<Option<Runtime>> = OnceLock::new();
    RT.get_or_init(|| {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().ok()?;
        if !root.join("vendor/bcapps/src/Layers/W1/BaseApp").exists() {
            return None;
        }
        let app = boot(&root, Db::Memory).unwrap();
        {
            let mut s = Session::new(&app.rt, "CRONUS");
            bc_server::seed::seed(&mut s).unwrap();
            s.commit().unwrap();
        }
        Some(app.rt)
    })
    .as_ref()
}

/// The tests here and in form_tests share `runtime()` and its in-memory store, where a session
/// sees rows another session wrote and has not committed (one writer with an undo log, readers
/// never block). A test that writes holds this lock exclusively until it has rolled back; the
/// others share it, so none of them reads another test's uncommitted rows.
pub(crate) static STORE: std::sync::RwLock<()> = std::sync::RwLock::new(());

/// `session!(s)`: a session on the shared runtime in `s`, holding `STORE` shared.
macro_rules! session {
    ($s:ident) => {
        let _store = STORE.read().unwrap_or_else(|e| e.into_inner());
        let mut $s = match runtime() {
            Some(rt) => Session::new(rt, "CRONUS"),
            None => {
                eprintln!("vendor/bcapps missing, skipping");
                return;
            }
        };
    };
}

fn fid(s: &Session, field: &str) -> i32 {
    let t = &s.cat.tables[&18];
    t.fields[t.field(field).unwrap()].id as i32
}

fn req(start: StartingPage, dir: ReadDirection, size: i32) -> GetPageRequest {
    GetPageRequest {
        page_request_definition: PageRequestDefinition {
            page_size: size,
            read_direction: dir,
            start_from_page: start,
            ..Default::default()
        },
        state: NavRecordState::initial(18, CUSTOMER_LIST as i32),
        ..Default::default()
    }
}

fn from(bookmark: &[u8], dir: ReadDirection, size: i32, exclude: bool) -> GetPageRequest {
    let mut r = req(StartingPage::Specific, dir, size);
    r.page_request_definition.starting_bookmark = Some(bookmark.to_vec());
    r.page_request_definition.exclude_starting_record = exclude;
    r
}

fn nos(t: &DataTable) -> Vec<String> {
    let c = t.column_index("1").expect("No. column");
    t.rows
        .iter()
        .map(|r| match &r[c] {
            Cell::String(s) => s.clone(),
            other => panic!("No. cell {other:?}"),
        })
        .collect()
}

fn bm(t: &DataTable, row: usize) -> Vec<u8> {
    match t.get(row, "bookmark") {
        Some(Cell::Bytes(b)) => b.clone(),
        other => panic!("bookmark cell {other:?}"),
    }
}

fn page(s: &mut Session, r: &GetPageRequest) -> GetPageResult {
    match get_page(s, CUSTOMER_LIST, r) {
        Ok(p) => p,
        Err(e) => panic!("get_page: {}", e.msg),
    }
}

fn v(xs: &[usize]) -> Vec<String> {
    xs.iter().map(|&i| no(i)).collect()
}

#[test]
fn forward_paging_by_bookmark() {
    session!(s);
    let p1 = page(&mut s, &req(StartingPage::First, ReadDirection::Next, 3));
    assert_eq!(p1.dataset.tables.len(), 1);
    let t = &p1.dataset.tables[0];
    assert_eq!(nos(t), v(&[0, 1, 2]));
    assert!(p1.more_data_in_read_direction);
    assert!(!p1.more_data_in_opposite_direction);
    assert!(p1.state.more_data_in_read_direction);

    // the client continues from the last row's bookmark, excluding it
    let p2 = page(&mut s, &from(&bm(t, 2), ReadDirection::Next, 3, true));
    assert_eq!(nos(&p2.dataset.tables[0]), v(&[3, 4, 5]));
    assert!(p2.more_data_in_read_direction);
    assert!(p2.more_data_in_opposite_direction);

    let p3 = page(&mut s, &from(&bm(&p2.dataset.tables[0], 2), ReadDirection::Next, 3, true));
    assert_eq!(nos(&p3.dataset.tables[0]), v(&[6, 7]));
    assert!(!p3.more_data_in_read_direction);
    assert!(p3.more_data_in_opposite_direction);

    // without ExcludeStartingRecord the bookmarked row comes first
    let p = page(&mut s, &from(&bm(t, 2), ReadDirection::Next, 2, false));
    assert_eq!(nos(&p.dataset.tables[0]), v(&[2, 3]));
}

#[test]
fn backward_paging_is_nearest_first() {
    session!(s);
    let first = page(&mut s, &req(StartingPage::First, ReadDirection::Next, 4));
    let at3 = bm(&first.dataset.tables[0], 3);
    let p = page(&mut s, &from(&at3, ReadDirection::Previous, 2, true));
    assert_eq!(nos(&p.dataset.tables[0]), v(&[2, 1]));
    assert!(p.more_data_in_read_direction); // C10000 is still before
    assert!(p.more_data_in_opposite_direction); // C10030 itself and later
    let p = page(&mut s, &from(&bm(&p.dataset.tables[0], 1), ReadDirection::Previous, 2, true));
    assert_eq!(nos(&p.dataset.tables[0]), v(&[0]));
    assert!(!p.more_data_in_read_direction);

    // Last + Previous: ExcludeStartingRecord does not drop the last row
    let mut r = req(StartingPage::Last, ReadDirection::Previous, 3);
    r.page_request_definition.exclude_starting_record = true;
    let p = page(&mut s, &r);
    assert_eq!(nos(&p.dataset.tables[0]), v(&[7, 6, 5]));
    assert!(p.more_data_in_read_direction);
    assert!(!p.more_data_in_opposite_direction);

    // Specific without a bookmark reads from the end when going backward
    let p = page(&mut s, &req(StartingPage::Specific, ReadDirection::Previous, 2));
    assert_eq!(nos(&p.dataset.tables[0]), v(&[7, 6]));
}

#[test]
fn initial_read_returns_two_tables() {
    session!(s);
    let all = page(&mut s, &req(StartingPage::First, ReadDirection::Next, 8));
    let at4 = bm(&all.dataset.tables[0], 4);
    let mut r = from(&at4, ReadDirection::Next, 2, false);
    r.page_request_definition.page_size_in_opposite_direction = 2;
    let p = page(&mut s, &r);
    assert_eq!(p.dataset.tables.len(), 2);
    assert_ne!(p.dataset.tables[0].name, p.dataset.tables[1].name);
    assert_eq!(nos(&p.dataset.tables[0]), v(&[4, 5]));
    assert_eq!(nos(&p.dataset.tables[1]), v(&[3, 2]));
    assert!(p.more_data_in_read_direction && p.more_data_in_opposite_direction);

    // near the start: the opposite table runs out
    let at1 = bm(&all.dataset.tables[0], 1);
    let mut r = from(&at1, ReadDirection::Next, 3, false);
    r.page_request_definition.page_size_in_opposite_direction = 5;
    let p = page(&mut s, &r);
    assert_eq!(nos(&p.dataset.tables[0]), v(&[1, 2, 3]));
    assert_eq!(nos(&p.dataset.tables[1]), v(&[0]));
    assert!(!p.more_data_in_opposite_direction);

    // IncludeMoreDataInformation = false leaves both flags false
    let mut r = req(StartingPage::First, ReadDirection::Next, 2);
    r.page_request_definition.include_more_data_information = false;
    let p = page(&mut s, &r);
    assert_eq!(nos(&p.dataset.tables[0]), v(&[0, 1]));
    assert!(!p.more_data_in_read_direction && !p.more_data_in_opposite_direction);
}

#[test]
fn columns_flowfields_and_bookmarks() {
    session!(s);
    let bal = fid(&s, "Balance (LCY)");
    let mut r = req(StartingPage::First, ReadDirection::Next, 8);
    r.page_request_definition.calc_fields = Some(vec![bal]);
    let p = page(&mut s, &r);
    let t = &p.dataset.tables[0];
    // the real NST's order (BC 29.1 capture)
    assert_eq!(&t.columns[0], &Column::new("bookmark", CellTypeCode::ByteArray));
    assert_eq!(&t.columns[1], &Column::new("suggestionAttributes", CellTypeCode::String));
    assert_eq!(&t.columns[2], &Column::new("0", CellTypeCode::Int64));
    let col = |name: &str| t.columns[t.column_index(name).unwrap_or_else(|| panic!("column {name}"))].ty;
    assert_eq!(col("1"), CellTypeCode::String); // No.
    assert_eq!(col(&fid(&s, "Blocked").to_string()), CellTypeCode::Int32); // enum
    assert_eq!(col(&fid(&s, "Credit Limit (LCY)").to_string()), CellTypeCode::Decimal);
    assert_eq!(col(&fid(&s, "Last Date Modified").to_string()), CellTypeCode::DateTime);
    assert_eq!(col(&bal.to_string()), CellTypeCode::Decimal);
    assert_eq!(col(&fid(&s, "Date Filter").to_string()), CellTypeCode::String); // FlowFilter
    // FlowFields not asked for keep their column (the client binds by name) and are DBLazy
    let due = fid(&s, "Balance Due (LCY)").to_string();
    assert!(t.columns[t.column_index(&due).unwrap()].lazy);
    assert_eq!(t.get(0, &due), Some(&Cell::Lazy));

    let schema = s.rt.schemas[&18].clone();
    for (i, (name, ..)) in CUSTOMERS.iter().enumerate() {
        assert_eq!(t.get(i, &bal.to_string()), Some(&Cell::Decimal(balance(i))), "balance of {}", no(i));
        assert_eq!(t.get(i, "2"), Some(&Cell::String(name.to_string())));
        assert_eq!(t.get(i, "suggestionAttributes"), Some(&Cell::Null));
        // the seed goes through Modify(true): OnModify sets Last Date Modified := Today
        // (midnight, kind Unspecified) and Last Modified Date Time := CurrentDateTime (UTC)
        assert_eq!(
            t.get(i, &fid(&s, "Last Date Modified").to_string()),
            Some(&convert::date_cell(al_runtime::value::today()))
        );
        match t.get(i, &fid(&s, "Last Modified Date Time").to_string()) {
            Some(Cell::DateTime {
                ticks,
                kind: DateTimeKind::Utc,
            }) => {
                let ms = convert::ticks_to_datetime(*ticks);
                assert!((chrono::Utc::now().timestamp_millis() - ms).abs() < 3_600_000, "{ms}");
            }
            other => panic!("Last Modified Date Time {other:?}"),
        }
        assert_eq!(decode_bookmark(&schema, &bm(t, i)).unwrap(), vec![Value::Text(no(i))]);
        assert!(matches!(t.get(i, "0"), Some(Cell::Int64(v)) if *v >= 0));
    }
    // the row version is stable while the row is unchanged, and differs between rows
    let again = page(&mut s, &r);
    assert_eq!(again.dataset.tables[0].get(0, "0"), t.get(0, "0"));
    assert_ne!(t.get(0, "0"), t.get(1, "0"));
    // the reply state carries the effective sorting
    let tv = p.state.table_view.as_ref().unwrap();
    assert_eq!(tv.current_sorting_field_ids, Some(vec![1]));
    assert!(tv.ascending);

    // LoadFieldsHint narrows the loaded normal fields (the primary key always stays);
    // the others keep their column (the client binds rows by column name) as DBLazy
    let mut r = req(StartingPage::First, ReadDirection::Next, 1);
    r.page_request_definition.load_fields_hint = Some(vec![2]);
    let p = page(&mut s, &r);
    let t = &p.dataset.tables[0];
    assert!(!matches!(t.get(0, "1"), Some(Cell::Lazy)) && !matches!(t.get(0, "2"), Some(Cell::Lazy)));
    assert_eq!(t.get(0, "7"), Some(&Cell::Lazy));
    assert!(t.columns.iter().find(|c| c.name == "7").unwrap().lazy);
}

#[test]
fn filters_search_and_flowfield_filters() {
    session!(s);
    let city = fid(&s, "City");
    let name = fid(&s, "Name");
    let bal = fid(&s, "Balance (LCY)");

    let mut r = req(StartingPage::First, ReadDirection::Next, 10);
    r.state.table_view_mut().current_filters = vec![ViewFilterGroup {
        filter_group_no: 0,
        filters: vec![ViewFilter {
            filter_field: city,
            filter_value: Some("Atlanta|Chicago|Napa".into()),
            runtime_filter: 1,
            ..Default::default()
        }],
    }];
    let p = page(&mut s, &r);
    assert_eq!(nos(&p.dataset.tables[0]), v(&[0, 1, 5]));
    // filters come back in the reply state unchanged
    assert_eq!(
        p.state.table_view.as_ref().unwrap().current_filters,
        r.state.table_view.as_ref().unwrap().current_filters
    );

    // paging under a filter
    let mut r2 = r.clone();
    r2.page_request_definition.page_size = 2;
    let p = page(&mut s, &r2);
    assert_eq!(nos(&p.dataset.tables[0]), v(&[0, 1]));
    assert!(p.more_data_in_read_direction);
    r2.page_request_definition.start_from_page = StartingPage::Specific;
    r2.page_request_definition.starting_bookmark = Some(bm(&p.dataset.tables[0], 1));
    r2.page_request_definition.exclude_starting_record = true;
    let p = page(&mut s, &r2);
    assert_eq!(nos(&p.dataset.tables[0]), v(&[5]));
    assert!(!p.more_data_in_read_direction);

    // search box: group -1 ORs across fields
    let mut r = req(StartingPage::First, ReadDirection::Next, 10);
    r.state.table_view_mut().search_filter = Some(ViewFilterGroup {
        filter_group_no: -1,
        filters: vec![
            ViewFilter {
                filter_field: name,
                filter_value: Some("@*ski*".into()),
                ..Default::default()
            },
            ViewFilter {
                filter_field: city,
                filter_value: Some("@*berlin*".into()),
                ..Default::default()
            },
        ],
    });
    let p = page(&mut s, &r);
    assert_eq!(nos(&p.dataset.tables[0]), v(&[3, 6]));

    // filter on a FlowField
    let mut r = req(StartingPage::First, ReadDirection::Next, 10);
    r.page_request_definition.calc_fields = Some(vec![bal]);
    r.state.table_view_mut().current_filters = vec![ViewFilterGroup {
        filter_group_no: 0,
        filters: vec![ViewFilter {
            filter_field: bal,
            filter_value: Some(">8000".into()),
            ..Default::default()
        }],
    }];
    let p = page(&mut s, &r);
    let expect: Vec<usize> = (0..CUSTOMERS.len()).filter(|&i| balance(i) > Decimal::from(8000)).collect();
    assert!(!expect.is_empty() && expect.len() < CUSTOMERS.len(), "{expect:?}");
    assert_eq!(nos(&p.dataset.tables[0]), v(&expect));

    // exact value
    let mut r = req(StartingPage::First, ReadDirection::Next, 10);
    r.state.table_view_mut().current_filters = vec![ViewFilterGroup {
        filter_group_no: 0,
        filters: vec![ViewFilter {
            filter_field: city,
            filter_value: Some("Miami".into()),
            is_exact_value: true,
            ..Default::default()
        }],
    }];
    assert_eq!(nos(&page(&mut s, &r).dataset.tables[0]), v(&[2]));
}

#[test]
fn sorting_by_key_and_in_memory() {
    session!(s);
    // descending on the primary key
    let mut r = req(StartingPage::First, ReadDirection::Next, 3);
    r.state.table_view_mut().current_sorting_field_ids = Some(vec![1]);
    r.state.table_view_mut().ascending = false;
    let p = page(&mut s, &r);
    assert_eq!(nos(&p.dataset.tables[0]), v(&[7, 6, 5]));
    assert!(!p.state.table_view.as_ref().unwrap().ascending);
    let next = {
        let mut n = r.clone();
        n.page_request_definition.start_from_page = StartingPage::Specific;
        n.page_request_definition.starting_bookmark = Some(bm(&p.dataset.tables[0], 2));
        n.page_request_definition.exclude_starting_record = true;
        n
    };
    assert_eq!(nos(&page(&mut s, &next).dataset.tables[0]), v(&[4, 3, 2]));

    // City has a key
    let city = fid(&s, "City");
    let mut r = req(StartingPage::First, ReadDirection::Next, 8);
    r.state.table_view_mut().current_sorting_field_ids = Some(vec![city]);
    let p = page(&mut s, &r);
    let mut by_city: Vec<usize> = (0..CUSTOMERS.len()).collect();
    by_city.sort_by(|&a, &b| CUSTOMERS[a].2.cmp(CUSTOMERS[b].2));
    assert_eq!(nos(&p.dataset.tables[0]), v(&by_city));
    assert_eq!(p.state.table_view.as_ref().unwrap().current_sorting_field_ids, Some(vec![city]));

    // Address leads no key: sorted in memory, still pageable by bookmark both ways
    let addr = fid(&s, "Address");
    let mut by_addr: Vec<usize> = (0..CUSTOMERS.len()).collect();
    by_addr.sort_by(|&a, &b| CUSTOMERS[a].1.to_lowercase().cmp(&CUSTOMERS[b].1.to_lowercase()).then(a.cmp(&b)));
    let mut r = req(StartingPage::First, ReadDirection::Next, 3);
    r.state.table_view_mut().current_sorting_field_ids = Some(vec![addr]);
    let p = page(&mut s, &r);
    assert_eq!(nos(&p.dataset.tables[0]), v(&by_addr[..3]));
    let mut n = r.clone();
    n.page_request_definition.start_from_page = StartingPage::Specific;
    n.page_request_definition.starting_bookmark = Some(bm(&p.dataset.tables[0], 2));
    n.page_request_definition.exclude_starting_record = true;
    let p2 = page(&mut s, &n);
    assert_eq!(nos(&p2.dataset.tables[0]), v(&by_addr[3..6]));
    n.page_request_definition.read_direction = ReadDirection::Previous;
    n.page_request_definition.starting_bookmark = Some(bm(&p2.dataset.tables[0], 0));
    let back = page(&mut s, &n);
    assert_eq!(nos(&back.dataset.tables[0]), v(&[by_addr[2], by_addr[1], by_addr[0]]));
}

#[test]
fn missing_and_foreign_bookmarks() {
    session!(s);
    let schema = s.rt.schemas[&18].clone();
    // a customer that does not exist still positions on the primary key
    let ghost = encode_pk(18, &[Value::Text("C10035".into())]);
    let p = page(&mut s, &from(&ghost, ReadDirection::Next, 2, false));
    assert_eq!(nos(&p.dataset.tables[0]), v(&[4, 5]));
    assert!(!p.bookmark_fallback);
    let p = page(&mut s, &from(&ghost, ReadDirection::Previous, 2, false));
    assert_eq!(nos(&p.dataset.tables[0]), v(&[3, 2]));

    // a bookmark of another table falls back to the first page
    let other = encode_pk(27, &[Value::Text("1000".into())]);
    let p = page(&mut s, &from(&other, ReadDirection::Next, 2, true));
    assert_eq!(nos(&p.dataset.tables[0]), v(&[0, 1]));
    assert!(p.bookmark_fallback);

    // card page style: one record by bookmark
    let one = encode_bookmark(&schema, &bookmark_row(&schema, &encode_pk(18, &[Value::Text(no(6))])).unwrap());
    let p = page(&mut s, &from(&one, ReadDirection::Next, 1, false));
    assert_eq!(nos(&p.dataset.tables[0]), v(&[6]));
    assert!(p.more_data_in_read_direction && p.more_data_in_opposite_direction);

    // a bookmarked record outside the filters gives a position but is not returned
    let mut r = from(&one, ReadDirection::Next, 5, false);
    r.state.table_view_mut().current_filters = vec![ViewFilterGroup {
        filter_group_no: 0,
        filters: vec![ViewFilter {
            filter_field: fid(&s, "Country/Region Code"),
            filter_value: Some("US".into()),
            ..Default::default()
        }],
    }];
    let p = page(&mut s, &r);
    assert_eq!(nos(&p.dataset.tables[0]), v(&[7]));
    assert!(p.more_data_in_opposite_direction);
}

#[test]
fn lookup_start_position() {
    session!(s);
    let mut r = req(StartingPage::Specific, ReadDirection::Next, 2);
    r.page_request_definition.lookup_field_ids = Some(vec![fid(&s, "City")]);
    r.page_request_definition.lookup_field_values = Some(serde_json::json!([{ "typeHint": 21, "value": "Copenhagen" }]));
    let p = page(&mut s, &r);
    assert_eq!(nos(&p.dataset.tables[0]), v(&[4, 5]));
}

#[test]
fn reply_survives_the_wire() {
    session!(s);
    let bal = fid(&s, "Balance (LCY)");
    let mut r = req(StartingPage::First, ReadDirection::Next, 5);
    r.page_request_definition.calc_fields = Some(vec![bal]);
    r.page_request_definition.page_size_in_opposite_direction = 1;
    let p = page(&mut s, &r);
    let ds = p.dataset.clone();
    let text = serde_json::to_string(&p.into_response()).unwrap();
    assert!(
        text.contains(r#""dataSet":{"compressed":false,"count":1,"buffers":["#),
        "{}",
        &text[..200.min(text.len())]
    );
    let back: DataSetResponse = serde_json::from_str(&text).unwrap();
    assert_eq!(back.data_set, ds);
    assert!(back.state.more_data_in_read_direction);
    // and compressed
    let w = ds.to_wire(Some(64)).unwrap();
    assert!(w.compressed);
    assert_eq!(NavDataSet::from_wire(&w).unwrap(), ds);
    // the request as the web server sends it (positional params)
    let params = serde_json::json!([
        serde_json::to_value(&r.page_request_definition).unwrap(),
        serde_json::to_value(&r.state).unwrap()
    ]);
    let again = GetPageRequest::from_params(&params).unwrap();
    assert_eq!(again.page_request_definition, r.page_request_definition);
    let p2 = page(&mut s, &again);
    assert_eq!(p2.dataset, ds);
}
