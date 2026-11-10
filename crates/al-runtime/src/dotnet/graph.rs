//! The Azure AD Graph client types the System Application uses in tests: MockGraphQuery (an
//! in-memory directory), GraphQuery(mock) wrapping it, the Licensing model property bags (UserInfo,
//! GroupInfo, ServicePlanInfo, SkuInfo, RoleInfo, TenantInfo, ...) and the paged results
//! (UserInfoPage, GroupInfoPage, UserIdsPage). Also the Dataverse CrmHelper proxy-version list.

use super::*;

pub const MODEL: &str = "Microsoft.Dynamics.Nav.LicensingService.Model.";
pub const CLIENT: &str = "Microsoft.Dynamics.Nav.AzureADGraphClient.";

#[derive(Default)]
pub struct Directory {
    users: Vec<NetRef>,
    groups: Vec<NetRef>,
    roles: Vec<NetRef>,
    skus: Vec<NetRef>,
    current_user: Option<NetRef>,
    collaboration: bool,
    env_group: Option<NetRef>,
}

pub struct Page {
    kind: &'static str,
    items: Vec<Val>,
    size: usize,
    pos: usize,
}

fn list(items: Vec<Val>) -> Val {
    net(NetObj::List("System.Collections.Generic.List`1", items))
}

/// A new property bag; collection properties start as empty lists.
pub fn bag(ty: &'static str) -> Val {
    let short = super::short_name(ty);
    let mut props: Vec<(String, Val)> = vec![];
    let lists: &[&str] = match short {
        "UserInfo" => &["AssignedPlans", "Roles", "Groups"],
        "SkuInfo" => &["ServicePlans"],
        "TenantInfo" => &["VerifiedDomains"],
        _ => &[],
    };
    for l in lists {
        props.push((l.to_string(), list(vec![])));
    }
    if short == "SkuInfo" {
        props.push(("PrepaidUnits".into(), bag(intern_model("LicenseUnitsInfo"))));
    }
    net(NetObj::Bag(ty, props))
}

fn intern_model(short: &str) -> &'static str {
    match short {
        "LicenseUnitsInfo" => "Microsoft.Dynamics.Nav.LicensingService.Model.LicenseUnitsInfo",
        _ => "Microsoft.Dynamics.Nav.LicensingService.Model.UserInfo",
    }
}

fn prop(n: &NetRef, name: &str) -> Val {
    match &*n.borrow() {
        NetObj::Bag(_, p) => p.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone()).unwrap_or(Val::Void),
        _ => Val::Void,
    }
}

fn prop_text(n: &NetRef, name: &str) -> String {
    prop(n, name).to_text()
}

/// Add to a bag's list property (AssignedPlans, Roles, Groups, ServicePlans).
fn push_to(n: &NetRef, list_name: &str, v: Val) {
    if let Val::Net(l) = prop(n, list_name) {
        if let NetObj::List(_, items) = &mut *l.borrow_mut() {
            items.push(v);
        }
    }
}

fn list_items(n: &NetRef, list_name: &str) -> Vec<Val> {
    match prop(n, list_name) {
        Val::Net(l) => match &*l.borrow() {
            NetObj::List(_, items) => items.clone(),
            _ => vec![],
        },
        _ => vec![],
    }
}

fn null(ty: &str) -> Val {
    net(NetObj::Null(super::resolve_type_name(ty)))
}

fn guid_key(v: &Val) -> String {
    let t = v.to_text();
    t.trim_matches(|c| c == '{' || c == '}').to_lowercase()
}

fn page(kind: &'static str, items: Vec<Val>, size: i64) -> Val {
    net(NetObj::GraphPage(Page { kind, items, size: size.max(1) as usize, pos: 0 }))
}

pub fn construct(ty: &'static str, a: &Args) -> Val {
    match super::short_name(ty) {
        "MockGraphQuery" => net(NetObj::GraphMock(Box::default())),
        // GraphQuery(mock) delegates to the mock; without one there is no Graph (no network)
        "GraphQuery" => match a.net(0) {
            Some(m) if matches!(&*m.borrow(), NetObj::GraphMock(_)) => Val::Net(m),
            _ => net(NetObj::Null(ty)),
        },
        "CrmHelper" => net(NetObj::Prim(ty, Val::Void)),
        _ if ty.starts_with(MODEL) => bag(ty),
        _ => net(NetObj::Null(ty)),
    }
}

fn arg_net(a: &Args, i: usize) -> Option<NetRef> {
    a.net(i).filter(|n| !matches!(&*n.borrow(), NetObj::Null(_)))
}

pub fn call(s: &mut Session, f: &mut Frame, n: &NetRef, ty: &'static str, m: &str, a: &Args) -> R<Out> {
    if ty.ends_with(".CrmHelper") {
        return match m {
            // proxy versions of the Dataverse SDK the service tier ships (the last is current)
            "getproxyidlist" => ok(list(vec![Val::Int(9), Val::Int(91), Val::Int(100)])),
            _ => Ok(Out::Unknown),
        };
    }
    let set = a.len() == 1;
    let mut o = n.borrow_mut();
    match &mut *o {
        NetObj::Bag(_, props) => {
            // properties (also called with method syntax: ServicePlans(), AccountEnabled())
            if set {
                let v = a.raw(0).cloned().map(unboxed).unwrap_or(Val::Void);
                match props.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(m)) {
                    Some(e) => e.1 = v,
                    None => props.push((m.to_string(), v)),
                }
                return void();
            }
            let v = props.iter().find(|(k, _)| k.eq_ignore_ascii_case(m)).map(|(_, v)| v.clone());
            ok(v.unwrap_or(Val::Void))
        }
        NetObj::GraphPage(p) => {
            let cur = || list(p.items.iter().skip(p.pos).take(p.size).cloned().collect());
            match m {
                "currentpage" => ok(cur()),
                _ if m.starts_with("getnext") => {
                    p.pos += p.size;
                    ok(Val::Bool(p.pos < p.items.len()))
                }
                "count" => ok(Val::Int(p.items.len() as i32)),
                _ => {
                    let _ = p.kind;
                    Ok(Out::Unknown)
                }
            }
        }
        NetObj::GraphMock(d) => {
            let find_user = |d: &Directory, f: &dyn Fn(&NetRef) -> bool| d.users.iter().find(|u| f(u)).cloned();
            let user_in_group = |u: &NetRef, pred: &dyn Fn(&NetRef) -> bool| list_items(u, "Groups").iter().any(|g| matches!(g, Val::Net(g) if pred(g)));
            let r = match m {
                // ---- mock setup
                "adduser" => {
                    if let Some(u) = arg_net(a, 0) {
                        d.users.push(u);
                    }
                    Val::Void
                }
                "addassignedplantouser" => {
                    if let (Some(u), Some(p)) = (arg_net(a, 0), a.raw(1)) {
                        push_to(&u, "AssignedPlans", p.clone());
                    }
                    Val::Void
                }
                "adduserrole" => {
                    if let (Some(u), Some(r)) = (arg_net(a, 0), a.raw(1)) {
                        push_to(&u, "Roles", r.clone());
                    }
                    Val::Void
                }
                "addusergroup" => {
                    if let (Some(u), Some(g)) = (arg_net(a, 0), arg_net(a, 1)) {
                        push_to(&u, "Groups", Val::Net(g.clone()));
                        if !d.groups.iter().any(|x| Rc::ptr_eq(x, &g)) {
                            d.groups.push(g);
                        }
                    }
                    Val::Void
                }
                "addgroup" => {
                    if let Some(g) = arg_net(a, 0) {
                        d.groups.push(g);
                    }
                    Val::Void
                }
                "adddirectoryrole" => {
                    if let Some(r) = arg_net(a, 0) {
                        d.roles.push(r);
                    }
                    Val::Void
                }
                "adddirectorysubscribedsku" => {
                    if let Some(k) = arg_net(a, 0) {
                        d.skus.push(k);
                    }
                    Val::Void
                }
                "currentuseruserobject" if set => {
                    d.current_user = arg_net(a, 0);
                    Val::Void
                }
                "m365collaborationenabled" if set => {
                    d.collaboration = a.bool(0);
                    Val::Void
                }
                "currentenvironmentsecuritygroup" if set => {
                    d.env_group = arg_net(a, 0);
                    Val::Void
                }
                // ---- GraphQuery API
                "getcurrentuser" | "currentuseruserobject" => d.current_user.clone().map(Val::Net).unwrap_or_else(|| null("UserInfo")),
                "getuser" => {
                    let upn = a.text(0);
                    find_user(d, &|u| prop_text(u, "UserPrincipalName").eq_ignore_ascii_case(&upn)).map(Val::Net).unwrap_or_else(|| null("UserInfo"))
                }
                "getuserbyauthorizationemail" => {
                    let e = a.text(0);
                    find_user(d, &|u| prop_text(u, "UserPrincipalName").eq_ignore_ascii_case(&e) || prop_text(u, "Mail").eq_ignore_ascii_case(&e)).map(Val::Net).unwrap_or_else(|| null("UserInfo"))
                }
                "getuserbyobjectid" | "trygetuserbyobjectid" => {
                    let id = a.text(0);
                    let u = find_user(d, &|u| prop_text(u, "ObjectId").eq_ignore_ascii_case(&id));
                    if m == "trygetuserbyobjectid" {
                        let found = u.is_some();
                        drop(o);
                        s.net_out(f, a, 1, u.map(Val::Net).unwrap_or_else(|| null("UserInfo")))?;
                        return ok(Val::Bool(found));
                    }
                    u.map(Val::Net).unwrap_or_else(|| null("UserInfo"))
                }
                "getuserassignedplans" => list(arg_net(a, 0).map(|u| list_items(&u, "AssignedPlans")).unwrap_or_default()),
                "getuserroles" => list(arg_net(a, 0).map(|u| list_items(&u, "Roles")).unwrap_or_default()),
                "getusergroups" => list(arg_net(a, 0).map(|u| list_items(&u, "Groups")).unwrap_or_default()),
                "getdirectorysubscribedskus" => list(d.skus.iter().cloned().map(Val::Net).collect()),
                "getdirectoryroles" => list(d.roles.iter().cloned().map(Val::Net).collect()),
                "gettenantdetail" => {
                    let t = bag("Microsoft.Dynamics.Nav.LicensingService.Model.TenantInfo");
                    if let Val::Net(tn) = &t {
                        if let NetObj::Bag(_, p) = &mut *tn.borrow_mut() {
                            p.push(("CountryLetterCode".into(), Val::Text("US".into())));
                            p.push(("PreferredLanguage".into(), Val::Text("en-US".into())));
                            p.push(("DisplayName".into(), Val::Text("Mock tenant".into())));
                        }
                    }
                    t
                }
                "ism365collaborationenabled" => Val::Bool(d.collaboration),
                "getenvironmentdirectorygroup" => Val::Text(d.env_group.as_ref().map(|g| prop_text(g, "ObjectId")).unwrap_or_default()),
                "getuserspage" => page("users", d.users.iter().cloned().map(Val::Net).collect(), a.int(0)),
                "getlicenseduserspage" => {
                    let plans: Vec<String> = super::enumerate_val(a.raw(0)).unwrap_or_default().iter().map(guid_key).collect();
                    let users = d
                        .users
                        .iter()
                        .filter(|u| list_items(u, "AssignedPlans").iter().any(|p| matches!(p, Val::Net(p) if plans.contains(&guid_key(&prop(p, "ServicePlanId"))))))
                        .cloned()
                        .map(Val::Net)
                        .collect();
                    page("users", users, a.int(1))
                }
                "getgroupmembers" => {
                    let name = a.text(0);
                    list(d.users.iter().filter(|u| user_in_group(u, &|g| prop_text(g, "DisplayName") == name)).cloned().map(Val::Net).collect())
                }
                "getmemberspageforgroupid" | "getmemberidspageforgroupid" => {
                    let id = a.text(0);
                    let members: Vec<NetRef> = d.users.iter().filter(|u| user_in_group(u, &|g| prop_text(g, "ObjectId") == id)).cloned().collect();
                    if m == "getmemberidspageforgroupid" {
                        page("ids", members.iter().map(|u| prop(u, "ObjectId")).collect(), a.int(1))
                    } else {
                        page("users", members.into_iter().map(Val::Net).collect(), a.int(1))
                    }
                }
                "isgroupmember" => {
                    let (uid, gid) = (a.text(0), a.text(1));
                    Val::Bool(d.users.iter().any(|u| prop_text(u, "ObjectId") == uid && user_in_group(u, &|g| prop_text(g, "ObjectId") == gid)))
                }
                "getgroupinfo" => {
                    let id = a.text(0);
                    d.groups.iter().find(|g| prop_text(g, "ObjectId") == id).cloned().map(Val::Net).unwrap_or_else(|| null("GroupInfo"))
                }
                "findfirstgroupinfofordisplayname" => {
                    let name = a.text(0);
                    d.groups.iter().find(|g| prop_text(g, "DisplayName") == name).cloned().map(Val::Net).unwrap_or_else(|| null("GroupInfo"))
                }
                "getgrouppage" => page("groups", d.groups.iter().cloned().map(Val::Net).collect(), a.int(0)),
                _ => return Ok(Out::Unknown),
            };
            ok(r)
        }
        _ => Ok(Out::Unknown),
    }
}
