//! Service-tier helper types that only exist as DotNet in BC: PermissionTestHelper (Permissions
//! Mock; permissions are not enforced here), NavUserAccountHelper.GetEffectivePermissionForObject
//! (test sessions run as SUPER), the Azure Key Vault client helper with the MockAzureKeyVault
//! secret provider tests plug in, and System.IO.MemoryMappedFiles (per-thread named buffers).

use super::*;

pub const PERMISSION_TEST_HELPER: &str = "Microsoft.Dynamics.Nav.Runtime.PermissionTestHelper";
pub const USER_ACCOUNT_HELPER: &str = "Microsoft.Dynamics.Nav.NavUserAccount.NavUserAccountHelper";
pub const KEY_VAULT_MOCK: &str = "MockTest.MockAzureKeyVaultSecret.MockAzureKeyVaultSecretProvider";
pub const KEY_VAULT_CLIENT: &str = "Microsoft.Dynamics.Nav.AzureKeyVaultClient.AzureKeyVaultClientHelper";
pub const MMF: &str = "System.IO.MemoryMappedFiles.MemoryMappedFile";
pub const MMF_VIEW: &str = "System.IO.MemoryMappedFiles.MemoryMappedViewStream";

pub type Buffer = Rc<RefCell<Vec<u8>>>;

thread_local! {
    static MAPPED: RefCell<Vec<(String, Buffer)>> = const { RefCell::new(vec![]) };
}

pub fn is_nav_type(t: &str) -> bool {
    matches!(t, PERMISSION_TEST_HELPER | USER_ACCOUNT_HELPER | KEY_VAULT_MOCK | KEY_VAULT_CLIENT | MMF | MMF_VIEW)
}

pub fn construct(ty: &'static str, a: &Args) -> Val {
    let _ = a;
    match ty {
        KEY_VAULT_MOCK => net(NetObj::KeyVaultMock(vec![], vec![])),
        KEY_VAULT_CLIENT => net(NetObj::KeyVaultClient(None)),
        _ => net(NetObj::Prim(ty, Val::Void)),
    }
}

fn mapped(name: &str) -> Option<Buffer> {
    MAPPED.with(|m| m.borrow().iter().find(|(n, _)| n == name).map(|(_, b)| b.clone()))
}

pub fn call(s: &mut Session, n: &NetRef, ty: &'static str, m: &str, a: &Args) -> R<Out> {
    let _ = s;
    match (ty, m) {
        // PermissionTestHelper (Microsoft.Dynamics.Nav.PermissionTestHelper.dll): the test's
        // effective permission sets (NavTestExecution.EffectivePermissionSets)
        (PERMISSION_TEST_HELPER, "clear" | "dispose") => {
            s.sys.perm.clear_effective();
            return void();
        }
        (PERMISSION_TEST_HELPER, "addeffectivepermissionset") => {
            s.sys.perm.add_effective(&a.text(0));
            return void();
        }
        (PERMISSION_TEST_HELPER, "removeeffectivepermissionset" | "setcurrentuser" | "enablefullalfunctiontracing") => return void(),
        // Read, Insert, Modify, Delete, Execute as Permission option ordinals (1 = Yes)
        (USER_ACCOUNT_HELPER, "geteffectivepermissionforobject") => return ok(Val::Text("1,1,1,1,1".into())),
        (USER_ACCOUNT_HELPER, "isazure") => return ok(Val::Bool(s.sys.env.kind.is_azure())),
        (MMF, "createoropen" | "createnew") => {
            let name = a.text(0);
            let cap = a.int(1).max(0) as usize;
            let buf = match mapped(&name) {
                Some(b) if m == "createoropen" => b,
                Some(_) => return net_err("MemoryMappedFile", "CreateNew", "Cannot create a file when that file already exists."),
                None => {
                    let b: Buffer = Rc::new(RefCell::new(vec![0; cap]));
                    MAPPED.with(|x| x.borrow_mut().push((name.clone(), b.clone())));
                    b
                }
            };
            return ok(net(NetObj::Mmf(name, buf)));
        }
        (MMF, "openexisting") => {
            let name = a.text(0);
            return match mapped(&name) {
                Some(b) => ok(net(NetObj::Mmf(name, b))),
                None => net_err("MemoryMappedFile", "OpenExisting", "Unable to find the specified file."),
            };
        }
        _ => {}
    }
    let mut o = n.borrow_mut();
    match &mut *o {
        NetObj::KeyVaultMock(secrets, certs) => ok(match m {
            "addsecretmapping" | "addsecretmappingfromfile" => {
                secrets.retain(|(k, _)| *k != a.text(0));
                secrets.push((a.text(0), a.text(1)));
                Val::Void
            }
            "addcertificatemapping" => {
                certs.retain(|(k, _)| *k != a.text(0));
                certs.push((a.text(0), a.text(1)));
                Val::Void
            }
            "getsecret" => Val::Text(secrets.iter().find(|(k, _)| *k == a.text(0)).map(|(_, v)| v.clone()).unwrap_or_default()),
            "getcertificate" => Val::Text(certs.iter().find(|(k, _)| *k == a.text(0)).map(|(_, v)| v.clone()).unwrap_or_default()),
            _ => return Ok(Out::Unknown),
        }),
        NetObj::KeyVaultClient(provider) => match m {
            "setazurekeyvaultprovider" => {
                *provider = a.net(0).filter(|p| matches!(&*p.borrow(), NetObj::KeyVaultMock(..)));
                void()
            }
            "getazurekeyvaultsecret" | "getazurekeyvaultcertificate" => {
                let name = a.text(0);
                let Some(p) = provider.clone() else {
                    // no vault on this server: nothing is configured
                    return ok(Val::Text(String::new()));
                };
                drop(o);
                let found = match &*p.borrow() {
                    NetObj::KeyVaultMock(secrets, certs) => {
                        let list = if m == "getazurekeyvaultsecret" { secrets } else { certs };
                        list.iter().find(|(k, _)| *k == name).map(|(_, v)| v.clone())
                    }
                    _ => None,
                };
                match found.filter(|v| !v.is_empty()) {
                    Some(v) => ok(Val::Text(v)),
                    None => err(format!("{name} is either missing or empty")),
                }
            }
            _ => Ok(Out::Unknown),
        },
        NetObj::Mmf(name, buf) => ok(match m {
            "createviewstream" => net(NetObj::MmfView(buf.clone(), 0)),
            "dispose" => {
                let name = name.clone();
                MAPPED.with(|x| x.borrow_mut().retain(|(n, _)| *n != name));
                Val::Void
            }
            _ => return Ok(Out::Unknown),
        }),
        NetObj::MmfView(buf, pos) => ok(match m {
            "copyto" => {
                let bytes = buf.borrow()[(*pos).min(buf.borrow().len())..].to_vec();
                *pos += bytes.len();
                drop(o);
                match a.raw(0) {
                    // an InStream on a Temp Blob shares the blob: it now reads the copied bytes
                    Some(Val::InStream(st)) => {
                        let mut st = st.borrow_mut();
                        st.data = Rc::new(bytes);
                        st.pos = 0;
                    }
                    Some(t) => super::text::stream_write(t, &bytes),
                    None => {}
                }
                Val::Void
            }
            "length" | "capacity" => Val::BigInt(buf.borrow().len() as i64),
            "position" if a.len() == 1 => {
                *pos = a.int(0).max(0) as usize;
                Val::Void
            }
            "position" => Val::BigInt(*pos as i64),
            "flush" | "dispose" | "close" => Val::Void,
            _ => return Ok(Out::Unknown),
        }),
        _ => Ok(Out::Unknown),
    }
}

/// CopyStream / Write into a view: overwrite the mapped buffer from the position (it does not grow).
pub fn view_write(buf: &Buffer, pos: &mut usize, bytes: &[u8]) {
    let mut b = buf.borrow_mut();
    for x in bytes {
        if *pos < b.len() {
            b[*pos] = *x;
        }
        *pos += 1;
    }
}
