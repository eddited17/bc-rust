//! System.Array and the System.Collections(.Generic/.Specialized) types AL uses: List`1,
//! ArrayList, Queue, Stack, Dictionary`2, Hashtable, SortedDictionary`2, HashSet`1,
//! KeyValuePair`2, IEnumerator, NameValueCollection.

use super::*;

fn key_eq(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Text(x), Val::Text(y)) => x == y,
        _ => equals(&unboxed(a.clone()), &unboxed(b.clone())),
    }
}

fn arr(t: &'static str, v: Vec<Val>) -> Val {
    net(NetObj::Array(t, v))
}

fn idx_err<T>(t: &str, m: &str) -> R<T> {
    net_err(t, m, "Index was out of range. Must be non-negative and less than the size of the collection. (Parameter 'index')")
}

pub fn call(s: &mut Session, f: &mut Frame, n: &NetRef, ty: &'static str, m: &str, a: &Args) -> R<Out> {
    // statics
    match (ty, m) {
        ("System.Array", "createinstance") => {
            let len = a.int(a.len() - 1).max(0) as usize;
            let et = match a.raw(0) {
                Some(Val::Text(t)) => super::resolve_type_name(t),
                _ => "System.Object",
            };
            let def = match et {
                "System.String" => Val::Void,
                "System.Int32" | "System.Byte" | "System.Int16" => Val::Int(0),
                "System.Int64" => Val::BigInt(0),
                "System.Boolean" => Val::Bool(false),
                "System.Char" => Val::Char('\0'),
                "System.Decimal" | "System.Double" => Val::Dec(Default::default()),
                _ => Val::Void,
            };
            return ok(arr(et, vec![def; len]));
        }
        ("System.Array", "indexof") => {
            let items = match a.raw(0) {
                Some(Val::Net(x)) => match &*x.borrow() {
                    NetObj::Array(_, v) => v.clone(),
                    _ => vec![],
                },
                _ => vec![],
            };
            let v = a.get(1);
            return ok(Val::Int(items.iter().position(|x| key_eq(x, &v)).map(|i| i as i32).unwrap_or(-1)));
        }
        ("System.Array", "sort") | ("System.Array", "reverse") | ("System.Array", "clear") => {
            if let Some(Val::Net(x)) = a.raw(0) {
                if let NetObj::Array(_, v) = &mut *x.borrow_mut() {
                    match m {
                        "sort" => v.sort_by(|p, q| compare(&unboxed(p.clone()), &unboxed(q.clone()))),
                        "reverse" => v.reverse(),
                        _ => v.iter_mut().for_each(|e| *e = Val::Void),
                    }
                }
            }
            return void();
        }
        ("System.Array", "copy") => {
            let src = match a.raw(0) {
                Some(Val::Net(x)) => match &*x.borrow() {
                    NetObj::Array(_, v) => v.clone(),
                    _ => vec![],
                },
                _ => vec![],
            };
            let k = a.int(a.len() - 1).max(0) as usize;
            if let Some(Val::Net(x)) = a.raw(if a.len() == 5 { 2 } else { 1 }) {
                if let NetObj::Array(_, d) = &mut *x.borrow_mut() {
                    let (so, dof) = if a.len() == 5 { (a.int(1) as usize, a.int(3) as usize) } else { (0, 0) };
                    for i in 0..k {
                        if let (Some(v), true) = (src.get(so + i), dof + i < d.len()) {
                            d[dof + i] = v.clone();
                        }
                    }
                }
            }
            return void();
        }
        ("System.Array", "empty") => return ok(arr("System.Object", vec![])),
        _ => {}
    }
    let mut o = n.borrow_mut();
    match &mut *o {
        NetObj::Array(t, v) => {
            let t = *t;
            ok(match m {
                "length" | "count" | "longlength" => Val::Int(v.len() as i32),
                "getlength" => Val::Int(v.len() as i32),
                "rank" => Val::Int(1),
                "getvalue" | "get" | "item" | "get_item" => {
                    let i = a.int(0);
                    match v.get(i as usize).filter(|_| i >= 0) {
                        Some(x) => x.clone(),
                        None => {
                            drop(o);
                            return net_err("Array", "GetValue", "Index was outside the bounds of the array.");
                        }
                    }
                }
                "setvalue" | "set" => {
                    let i = a.int(1);
                    if i < 0 || i as usize >= v.len() {
                        drop(o);
                        return net_err("Array", "SetValue", "Index was outside the bounds of the array.");
                    }
                    v[i as usize] = a.get(0);
                    Val::Void
                }
                "clone" => arr(t, v.clone()),
                "copyto" => {
                    let src = v.clone();
                    drop(o);
                    if let Some(Val::Net(d)) = a.raw(0) {
                        if let NetObj::Array(_, dv) = &mut *d.borrow_mut() {
                            let off = a.int(1).max(0) as usize;
                            for (k, x) in src.into_iter().enumerate() {
                                if off + k < dv.len() {
                                    dv[off + k] = x;
                                }
                            }
                        }
                    }
                    Val::Void
                }
                "getenumerator" => net(NetObj::Enumerator(v.clone(), -1)),
                "contains" => Val::Bool(v.iter().any(|x| key_eq(x, &a.get(0)))),
                "indexof" => Val::Int(v.iter().position(|x| key_eq(x, &a.get(0))).map(|i| i as i32).unwrap_or(-1)),
                "tostring" => Val::Text(format!("{t}[]")),
                _ => return Ok(Out::Unknown),
            })
        }
        NetObj::List(t, v) => {
            let t = *t;
            ok(match m {
                "add" | "enqueue" | "push" => {
                    v.push(a.get(0));
                    if t.ends_with("ArrayList") {
                        Val::Int(v.len() as i32 - 1)
                    } else {
                        Val::Void
                    }
                }
                "addrange" => {
                    let items = super::enumerate_val(a.raw(0)).unwrap_or_default();
                    v.extend(items);
                    Val::Void
                }
                "count" => Val::Int(v.len() as i32),
                "item" | "get_item" | "elementat" => {
                    let i = a.int(0);
                    if a.len() == 2 {
                        if i < 0 || i as usize >= v.len() {
                            drop(o);
                            return idx_err("List", "Item");
                        }
                        v[i as usize] = a.get(1);
                        Val::Void
                    } else {
                        match v.get(i as usize).filter(|_| i >= 0) {
                            Some(x) => x.clone(),
                            None => {
                                drop(o);
                                return idx_err("List", "Item");
                            }
                        }
                    }
                }
                "contains" => Val::Bool(v.iter().any(|x| key_eq(x, &a.get(0)))),
                "indexof" => Val::Int(v.iter().position(|x| key_eq(x, &a.get(0))).map(|i| i as i32).unwrap_or(-1)),
                "remove" => {
                    let p = v.iter().position(|x| key_eq(x, &a.get(0)));
                    if let Some(p) = p {
                        v.remove(p);
                    }
                    Val::Bool(p.is_some())
                }
                "removeat" => {
                    let i = a.int(0);
                    if i < 0 || i as usize >= v.len() {
                        drop(o);
                        return idx_err("List", "RemoveAt");
                    }
                    v.remove(i as usize);
                    Val::Void
                }
                "insert" => {
                    let i = (a.int(0).max(0) as usize).min(v.len());
                    v.insert(i, a.get(1));
                    Val::Void
                }
                "clear" => {
                    v.clear();
                    Val::Void
                }
                "toarray" => arr("System.Object", v.clone()),
                "sort" => {
                    v.sort_by(|p, q| compare(&unboxed(p.clone()), &unboxed(q.clone())));
                    Val::Void
                }
                "reverse" => {
                    v.reverse();
                    Val::Void
                }
                "dequeue" => {
                    if v.is_empty() {
                        drop(o);
                        return net_err("Queue", "Dequeue", "Queue empty.");
                    }
                    v.remove(0)
                }
                "pop" => match v.pop() {
                    Some(x) => x,
                    None => {
                        drop(o);
                        return net_err("Stack", "Pop", "Stack empty.");
                    }
                },
                "peek" => match if t.ends_with("Stack") { v.last() } else { v.first() } {
                    Some(x) => x.clone(),
                    None => {
                        drop(o);
                        return net_err(super::short_name(t), "Peek", "Collection empty.");
                    }
                },
                "getenumerator" => net(NetObj::Enumerator(if t.ends_with("Stack") { v.iter().rev().cloned().collect() } else { v.clone() }, -1)),
                "capacity" => Val::Int(v.capacity().max(v.len()) as i32),
                "first" => match v.first() {
                    Some(x) => x.clone(),
                    None => {
                        drop(o);
                        return net_err("List", "First", "Sequence contains no elements");
                    }
                },
                _ => return Ok(Out::Unknown),
            })
        }
        NetObj::Dict(t, d) => {
            let t = *t;
            let hashtable = t.ends_with("Hashtable");
            let set = t.contains("HashSet");
            let sorted = t.contains("Sorted");
            let r = match m {
                "add" => {
                    let k = a.get(0);
                    if d.iter().any(|(x, _)| key_eq(x, &k)) {
                        if set {
                            return ok(Val::Bool(false));
                        }
                        drop(o);
                        return net_err(super::short_name(t), "Add", format!("An item with the same key has already been added. Key: {}", k.to_text()));
                    }
                    d.push((k, a.get(1)));
                    if sorted {
                        d.sort_by(|p, q| compare(&unboxed(p.0.clone()), &unboxed(q.0.clone())));
                    }
                    if set {
                        Val::Bool(true)
                    } else {
                        Val::Void
                    }
                }
                "trygetvalue" => {
                    let k = a.get(0);
                    let found = d.iter().find(|(x, _)| key_eq(x, &k)).map(|(_, v)| v.clone());
                    drop(o);
                    let r = found.is_some();
                    if let Some(v) = found {
                        s.net_out(f, a, 1, v)?;
                    }
                    return ok(Val::Bool(r));
                }
                "containskey" | "contains" => {
                    let k = a.get(0);
                    Val::Bool(d.iter().any(|(x, _)| key_eq(x, &k)))
                }
                "containsvalue" => {
                    let k = a.get(0);
                    Val::Bool(d.iter().any(|(_, x)| key_eq(x, &k)))
                }
                "remove" => {
                    let k = a.get(0);
                    let p = d.iter().position(|(x, _)| key_eq(x, &k));
                    if let Some(p) = p {
                        d.remove(p);
                    }
                    Val::Bool(p.is_some())
                }
                "count" => Val::Int(d.len() as i32),
                "clear" => {
                    d.clear();
                    Val::Void
                }
                "item" | "get_item" | "get" => {
                    let k = a.get(0);
                    if a.len() == 2 {
                        let v = a.get(1);
                        match d.iter_mut().find(|(x, _)| key_eq(x, &k)) {
                            Some(e) => e.1 = v,
                            None => d.push((k, v)),
                        }
                        if sorted {
                            d.sort_by(|p, q| compare(&unboxed(p.0.clone()), &unboxed(q.0.clone())));
                        }
                        Val::Void
                    } else {
                        match d.iter().find(|(x, _)| key_eq(x, &k)) {
                            Some((_, v)) => v.clone(),
                            None if hashtable => Val::Void,
                            None => {
                                drop(o);
                                return net_err(super::short_name(t), "Item", format!("The given key '{}' was not present in the dictionary.", k.to_text()));
                            }
                        }
                    }
                }
                "keys" => arr("System.Object", d.iter().map(|(k, _)| k.clone()).collect()),
                "values" => arr("System.Object", d.iter().map(|(_, v)| v.clone()).collect()),
                "getenumerator" => {
                    let items = if set { d.iter().map(|(k, _)| k.clone()).collect() } else { d.iter().map(|(k, v)| net(NetObj::KeyValue(k.clone(), v.clone()))).collect() };
                    net(NetObj::Enumerator(items, -1))
                }
                "toarray" => arr("System.Object", d.iter().map(|(k, _)| k.clone()).collect()),
                _ => return Ok(Out::Unknown),
            };
            ok(r)
        }
        NetObj::KeyValue(k, v) => ok(match m {
            "key" => k.clone(),
            "value" => v.clone(),
            _ => return Ok(Out::Unknown),
        }),
        NetObj::Enumerator(items, pos) => ok(match m {
            "movenext" => {
                *pos += 1;
                Val::Bool((*pos as usize) < items.len())
            }
            "current" => match items.get(*pos as usize).filter(|_| *pos >= 0) {
                Some(x) => x.clone(),
                None => {
                    drop(o);
                    return net_err("IEnumerator", "Current", "Enumeration has either not started or has already finished.");
                }
            },
            "reset" => {
                *pos = -1;
                Val::Void
            }
            "getenumerator" => {
                let it = items.clone();
                net(NetObj::Enumerator(it, -1))
            }
            "count" => Val::Int(items.len() as i32),
            _ => return Ok(Out::Unknown),
        }),
        NetObj::NameValueCollection(v) => {
            let key_index = |v: &Vec<(Option<String>, Vec<String>)>, k: &Val| -> Option<usize> {
                match k {
                    Val::Int(i) => Some(*i as usize).filter(|i| *i < v.len()),
                    Val::Void => v.iter().position(|(x, _)| x.is_none()),
                    k => {
                        let ks = k.to_text();
                        v.iter().position(|(x, _)| x.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(&ks)))
                    }
                }
            };
            ok(match m {
                "count" => Val::Int(v.len() as i32),
                "getkey" => match v.get(a.int(0) as usize) {
                    Some((Some(k), _)) => Val::Text(k.clone()),
                    Some((None, _)) => Val::Void,
                    None => {
                        drop(o);
                        return idx_err("NameValueCollection", "GetKey");
                    }
                },
                "getvalues" => match key_index(v, &a.get(0)) {
                    Some(i) => arr("System.String", v[i].1.iter().cloned().map(Val::Text).collect()),
                    None => Val::Void,
                },
                "get" | "item" | "get_item" => match key_index(v, &a.get(0)) {
                    Some(i) => Val::Text(v[i].1.join(",")),
                    None => Val::Void,
                },
                "allkeys" | "keys" => arr("System.String", v.iter().map(|(k, _)| k.clone().map(Val::Text).unwrap_or(Val::Void)).collect()),
                "add" => {
                    let k = match a.get(0) {
                        Val::Void => None,
                        x => Some(x.to_text()),
                    };
                    let val = a.text(1);
                    match v.iter_mut().find(|(x, _)| x.as_deref().map(|x| x.to_lowercase()) == k.as_deref().map(|x| x.to_lowercase())) {
                        Some(e) => e.1.push(val),
                        None => v.push((k, vec![val])),
                    }
                    Val::Void
                }
                "set" => {
                    let k = a.text(0);
                    let val = a.text(1);
                    match v.iter_mut().find(|(x, _)| x.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(&k))) {
                        Some(e) => e.1 = vec![val],
                        None => v.push((Some(k), vec![val])),
                    }
                    Val::Void
                }
                "remove" => {
                    let k = a.text(0);
                    v.retain(|(x, _)| !x.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(&k)));
                    Val::Void
                }
                "clear" => {
                    v.clear();
                    Val::Void
                }
                "tostring" => Val::Text(
                    v.iter()
                        .flat_map(|(k, vals)| vals.iter().map(move |x| match k {
                            Some(k) => format!("{}={}", super::uri::url_encode(k, false), super::uri::url_encode(x, false)),
                            None => super::uri::url_encode(x, false),
                        }))
                        .collect::<Vec<_>>()
                        .join("&"),
                ),
                _ => return Ok(Out::Unknown),
            })
        }
        NetObj::Null(_) => Ok(Out::Unknown),
        _ => Ok(Out::Unknown),
    }
}
