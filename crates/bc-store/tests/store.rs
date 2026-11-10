use bc_store::*;
use std::ops::Bound;
use std::sync::Arc;

fn schema() -> Arc<TableSchema> {
    Arc::new(TableSchema {
        id: 18,
        name: "Customer".into(),
        columns: vec![ColType::Code, ColType::Text, ColType::Dec],
        field_ids: vec![1, 2, 3],
        keys: vec![vec![0], vec![1]],
        per_company: true,
    })
}

fn row(no: &str, name: &str, amt: i64) -> Row {
    vec![Value::Text(no.into()), Value::Text(name.into()), Value::Dec(amt.into())]
}

#[test]
fn insert_scan_rollback_and_wal_replay() {
    let dir = std::env::temp_dir().join(format!("bcstore-test-{}", std::process::id()));
    let _ = std::fs::remove_file(&dir);
    let s = schema();
    let sc = s.clone();
    let resolver: Resolver = Box::new(move |id| (id == 18).then(|| sc.clone()));
    {
        let (db, n) = Store::open(&dir, &resolver).unwrap();
        assert_eq!(n, 0);
        let mut t = Txn::default();
        db.insert(&mut t, "CRONUS", &s, row("C2", "beta", 2)).unwrap();
        db.insert(&mut t, "CRONUS", &s, row("C1", "Alpha", 1)).unwrap();
        assert_eq!(db.insert(&mut t, "CRONUS", &s, row("c1", "dup", 1)), Err(StoreError::Duplicate));
        db.commit(&mut t).unwrap();
        // rolled back work must not survive
        db.insert(&mut t, "CRONUS", &s, row("C3", "gamma", 3)).unwrap();
        db.modify(&mut t, "CRONUS", &s, row("C1", "Changed", 9)).unwrap();
        db.rollback(&mut t);
        // secondary key order: Alpha, beta (case-insensitive)
        let mut names = vec![];
        db.scan("CRONUS", &s, 1, Bound::Unbounded, Bound::Unbounded, false, |_, r| {
            if let Value::Text(n) = &r[1] {
                names.push(n.clone());
            }
            true
        });
        assert_eq!(names, vec!["Alpha", "beta"]);
        db.delete(&mut t, "CRONUS", &s, &s.pk(&row("C2", "", 0))).unwrap();
        db.commit(&mut t).unwrap();
    }
    let (db, n) = Store::open(&dir, &resolver).unwrap();
    assert_eq!(n, 3);
    assert_eq!(db.count("CRONUS", &s), 1);
    let r = db.get("CRONUS", &s, &s.pk(&row("C1", "", 0))).unwrap();
    assert_eq!(r[1], Value::Text("Alpha".into()));
    db.compact().unwrap();
    drop(db);
    let (db, n) = Store::open(&dir, &resolver).unwrap();
    assert_eq!((n, db.count("CRONUS", &s)), (1, 1));
    let _ = std::fs::remove_file(&dir);
}
