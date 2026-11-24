//! API 0.10: the network a plugin reaches (declared hosts, asked once),
//! signing in by a device code, and lossless GFF JSON, against a stub
//! server that plays both the issuer and the service.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use base64::Engine;
use mg_edit::{Command, Workspace};
use mg_gff::{Gff, Struct, Value};
use mg_module::Module;
use mg_plugin::net::{Request, Response, Transport};
use mg_plugin::{
    Answer, Host, Input, Level, Network, Plugin, PluginError, Question, SignIns, run_check,
    run_command, run_console,
};
use mg_resman::ResKey;

fn docs() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs")
}

fn key(name: &str) -> ResKey {
    ResKey::from_filename(name).unwrap()
}

const ISSUER: &str = "https://auth.example.org/application/o/area-share";

/// The issuer and the service: what each request was, and what it got.
#[derive(Default)]
struct Stub {
    seen: Mutex<Vec<Request>>,
    /// Polls of the token endpoint left to answer "pending".
    pending: Mutex<u32>,
    /// The area the service holds, as nwn_gff JSON text.
    held: Mutex<Option<(String, String)>>,
    /// Access tokens handed out (each refresh a new one).
    issued: Mutex<u32>,
    /// The service refuses this access token (401), as an expired one.
    refuse: Mutex<Option<String>>,
}

fn reply(status: u16, body: serde_json::Value) -> Result<Response, String> {
    Ok(Response {
        status,
        headers: vec![("content-type".into(), "application/json".into())],
        body: body.to_string().into_bytes(),
    })
}

fn form_of(r: &Request) -> std::collections::BTreeMap<String, String> {
    let text = String::from_utf8(r.body.clone()).unwrap();
    let unescape = |s: &str| {
        let mut out = Vec::new();
        let b = s.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                i += 3;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
    };
    text.split('&')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (unescape(k), unescape(v)))
        .collect()
}

impl Stub {
    fn tokens(&self) -> Result<Response, String> {
        let mut n = self.issued.lock().unwrap();
        *n += 1;
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"sub":"u1","preferred_username":"alice"}"#);
        reply(
            200,
            serde_json::json!({
                "access_token": format!("AT{n}"), "refresh_token": format!("RT{n}"),
                "expires_in": 600, "id_token": format!("h.{claims}.s"),
            }),
        )
    }

    fn bearer(&self, r: &Request) -> Option<String> {
        let (_, v) = r.headers.iter().find(|(k, _)| k == "Authorization")?;
        v.strip_prefix("Bearer ").map(str::to_string)
    }
}

impl Transport for Stub {
    fn send(&self, r: &Request) -> Result<Response, String> {
        self.seen.lock().unwrap().push(r.clone());
        let url = r.url.as_str();
        if url == format!("{ISSUER}/.well-known/openid-configuration") {
            return reply(
                200,
                serde_json::json!({
                    "issuer": format!("{ISSUER}/"),
                    "device_authorization_endpoint": "https://auth.example.org/application/o/device/",
                    "token_endpoint": "https://auth.example.org/application/o/token/",
                    "revocation_endpoint": "https://auth.example.org/application/o/revoke/",
                }),
            );
        }
        if url.ends_with("/o/device/") {
            assert_eq!(form_of(r)["client_id"], "area-share");
            return reply(
                200,
                serde_json::json!({
                    "device_code": "DEV", "user_code": "WXYZ-1234",
                    "verification_uri": "https://auth.example.org/device",
                    "expires_in": 60, "interval": 1,
                }),
            );
        }
        if url.ends_with("/o/token/") {
            let f = form_of(r);
            if f["grant_type"] == "refresh_token" {
                return self.tokens();
            }
            let mut pending = self.pending.lock().unwrap();
            if *pending > 0 {
                *pending -= 1;
                return reply(400, serde_json::json!({ "error": "authorization_pending" }));
            }
            return self.tokens();
        }
        if url.ends_with("/o/revoke/") {
            return reply(200, serde_json::json!({}));
        }
        if let Some(area) = url.strip_prefix("https://areas.example.org/v1/areas/") {
            let token = self.bearer(r);
            let refused = self.refuse.lock().unwrap().clone();
            if token.is_none() || token == refused {
                return reply(401, serde_json::json!({ "detail": "no" }));
            }
            assert_eq!(area, "tavern");
            if r.method == "POST" {
                let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
                let text = |k: &str| body[k].to_string();
                *self.held.lock().unwrap() = Some((text("are"), text("git")));
                return reply(200, serde_json::json!({ "rev": 7 }));
            }
            let held = self.held.lock().unwrap().clone().unwrap();
            let body = format!("{{\"are\":{},\"git\":{}}}", held.0, held.1);
            return Ok(Response { status: 200, headers: Vec::new(), body: body.into_bytes() });
        }
        reply(404, serde_json::json!({}))
    }
}

/// A host with the stub as its network: it answers the questions it is
/// given, and waits out a sign-in's code as a window does.
struct NetHost {
    network: Option<Network>,
    answers: RefCell<VecDeque<Option<Answer>>>,
    asked: RefCell<Vec<Question>>,
    log: RefCell<Vec<String>>,
}

impl NetHost {
    fn new(stub: &Arc<Stub>, sign_ins: &Arc<SignIns>) -> NetHost {
        let network = Network::new(stub.clone(), sign_ins.clone());
        NetHost {
            network: Some(network),
            answers: RefCell::default(),
            asked: RefCell::default(),
            log: RefCell::default(),
        }
    }

    fn yes(self) -> NetHost {
        self.answers.borrow_mut().push_back(Some(Answer::Yes));
        self
    }
}

impl Host for NetHost {
    fn log(&self, _: Level, text: &str) {
        self.log.borrow_mut().push(text.to_string());
    }

    fn ask(&self, question: &Question) -> Option<Answer> {
        self.asked.borrow_mut().push(question.clone());
        if let Question::SignIn(code) = question {
            while !code.waiting.done() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            return Some(Answer::Yes);
        }
        self.answers.borrow_mut().pop_front().flatten()
    }

    fn network(&self) -> Option<Network> {
        self.network.clone()
    }
}

fn area_share() -> Plugin {
    Plugin::load(&docs().join("plugins/examples/area-share")).unwrap()
}

/// A module with the area `tavern`, its GIT holding 64-bit fields at
/// their ends, past what a Luau number holds.
fn module() -> Module {
    let mut m = Module::new();
    m.set_info(&Gff::new(*b"IFO ")).unwrap();
    let mut are = Gff::new(*b"ARE ");
    are.root.set("Tag", Value::String(b"TAVERN".to_vec()));
    are.root.set("Tile_List", Value::List(Vec::new()));
    m.set_gff(key("tavern.are"), &are).unwrap();
    let mut git = Gff::new(*b"GIT ");
    let mut p = Struct::new(9);
    p.set("Tag", Value::String(b"chest".to_vec()));
    p.set("Big", Value::Dword64(u64::MAX - 1));
    p.set("Low", Value::Int64(i64::MIN + 3));
    git.root.set("Placeable List", Value::List(vec![p]));
    m.set_gff(key("tavern.git"), &git).unwrap();
    m
}

fn on_tavern(m: &Module) -> Input {
    let mut input = Input::new(m.clone(), None);
    input.selection.selected = vec![mg_plugin::Selected::Area("tavern".parse().unwrap())];
    input
}

/// The example signs in (the user asked once for both hosts), sends the
/// area with its token, and fetches it back byte for byte.
#[test]
fn area_share_signs_in_sends_and_fetches_losslessly() {
    let stub = Arc::new(Stub::default());
    *stub.pending.lock().unwrap() = 1;
    let sign_ins = Arc::new(SignIns::in_memory());
    let plugin = area_share();
    let m = module();

    let host = Rc::new(NetHost::new(&stub, &sign_ins).yes());
    run_command(&plugin, "sign-in", on_tavern(&m), host.clone()).unwrap();
    assert_eq!(host.log.borrow().as_slice(), ["Signed in as alice"]);
    let asked = host.asked.borrow();
    let Question::Network { plugin: name, hosts } = &asked[0] else { panic!("{asked:?}") };
    assert_eq!(
        (name.as_str(), hosts.as_slice()),
        (
            "Area share",
            ["areas.example.org".to_string(), "auth.example.org".to_string()].as_slice()
        )
    );
    let Question::SignIn(code) = &asked[1] else { panic!("{asked:?}") };
    assert_eq!((code.code.as_str(), code.service.as_str()), ("WXYZ-1234", "auth.example.org"));
    drop(asked);

    // (A new job: asked again, as a host that keeps no answer has it.)
    let host = Rc::new(NetHost::new(&stub, &sign_ins).yes());
    run_command(&plugin, "send", on_tavern(&m), host.clone()).unwrap();
    assert_eq!(host.log.borrow().as_slice(), ["tavern: sent, revision 7"]);
    let posted = stub.seen.lock().unwrap().last().cloned().unwrap();
    assert!(posted.headers.contains(&("Authorization".into(), "Bearer AT1".into())));
    let held = stub.held.lock().unwrap().clone().unwrap();
    assert!(held.1.contains("18446744073709551614") && held.1.contains("-9223372036854775805"));

    // Fetched: the same bytes as were sent.
    let mut changed = Workspace::new(Module::new());
    changed.module.set_info(&Gff::new(*b"IFO ")).unwrap();
    let host = Rc::new(NetHost::new(&stub, &sign_ins).yes());
    let empty = changed.module.clone();
    let out = run_command(&plugin, "fetch", on_tavern(&empty), host).unwrap();
    changed.apply(Command::new(out.label, out.edits)).unwrap();
    changed.flush().unwrap();
    for name in ["tavern.are", "tavern.git"] {
        assert_eq!(changed.module.get(&key(name)), m.get(&key(name)), "{name}");
    }

    // Signed out: revoked at the issuer, and gone here.
    let host = Rc::new(NetHost::new(&stub, &sign_ins).yes());
    run_command(&plugin, "sign-out", on_tavern(&m), host).unwrap();
    let revoked = stub.seen.lock().unwrap().iter().any(|r| r.url.ends_with("/o/revoke/"));
    assert!(revoked, "the refresh token is revoked");
    let host = Rc::new(NetHost::new(&stub, &sign_ins).yes());
    let e = run_command(&plugin, "send", on_tavern(&m), host).unwrap_err();
    assert!(e.to_string().contains("not signed in"), "{e}");
}

/// A refused token is refreshed once, and the request sent again; the
/// plugin never sees either token.
#[test]
fn a_refused_token_is_refreshed_once() {
    let stub = Arc::new(Stub::default());
    let sign_ins = Arc::new(SignIns::in_memory());
    let plugin = area_share();
    let m = module();
    let host = Rc::new(NetHost::new(&stub, &sign_ins).yes());
    run_command(&plugin, "sign-in", on_tavern(&m), host).unwrap();
    *stub.refuse.lock().unwrap() = Some("AT1".into());
    let host = Rc::new(NetHost::new(&stub, &sign_ins).yes());
    run_command(&plugin, "send", on_tavern(&m), host).unwrap();
    let seen = stub.seen.lock().unwrap();
    let posts: Vec<&Request> =
        seen.iter().filter(|r| r.method == "POST" && r.url.contains("areas")).collect();
    assert_eq!(posts.len(), 2);
    assert!(posts[1].headers.contains(&("Authorization".into(), "Bearer AT2".into())));
    // (The console reaches no network: it declares no hosts.)
    let console = "return ctx.net:request({ url = 'https://areas.example.org/v1/areas/tavern' })";
    let host = Rc::new(NetHost::new(&stub, &sign_ins));
    let e = run_console(console, Input::new(m.clone(), None), host).unwrap_err();
    assert!(e.to_string().contains("is for commands and wizards"), "{e}");
}

/// What a plugin may not do: reach a host it does not declare, plain
/// HTTP, the network unasked or refused, or from a check.
#[test]
fn the_network_is_the_declared_hosts_with_leave_from_a_command() {
    let stub = Arc::new(Stub::default());
    let sign_ins = Arc::new(SignIns::in_memory());
    let dir = mg_testkit::scratch_dir("plugin-network-refused");
    std::fs::write(
        dir.join("plugin.cfg"),
        "[plugin]\nid = \"t.net\"\nname = \"Net\"\nversion = \"1\"\napi = \"0.10\"\n\
         license = \"MIT\"\n\n[network]\nhosts = \"areas.example.org\"\n\n\
         [command]\nid = \"get\"\ntitle = \"Get\"\n\n[check]\nid = \"c\"\ntitle = \"C\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.luau"),
        "local mg = require('@moonglow')\n\
         mg.command('get', function(ctx)\n\
           local r = ctx.net:request({ url = ctx.settings.url or URL })\n\
           ctx.log:info(r.status .. ' ' .. tostring(r.ok))\n\
         end)\n\
         mg.check('c', function(ctx)\n\
           ctx.net:request({ url = 'https://areas.example.org/x' })\n\
           return {}\n\
         end)\n",
    )
    .unwrap();
    let plugin = Plugin::load(&dir).unwrap();
    let m = module();
    let run = |url: &str, host: NetHost| -> Result<Rc<NetHost>, PluginError> {
        let main = std::fs::read_to_string(dir.join("main.luau")).unwrap();
        let fixed = main.replace("ctx.settings.url or URL", &format!("{url:?}"));
        let d2 = mg_testkit::scratch_dir("plugin-network-refused-2");
        std::fs::copy(dir.join("plugin.cfg"), d2.join("plugin.cfg")).unwrap();
        std::fs::write(d2.join("main.luau"), fixed).unwrap();
        let p = Plugin::load(&d2).unwrap();
        let host = Rc::new(host);
        run_command(&p, "get", Input::new(m.clone(), None), host.clone()).map(|_| host)
    };
    let fails = |url: &str, host: NetHost| run(url, host).err().unwrap().to_string();
    let net = || NetHost::new(&stub, &sign_ins);
    assert!(
        fails("https://evil.example.org/x", net().yes())
            .contains("not a host the manifest declares")
    );
    assert!(fails("http://areas.example.org/x", net().yes()).contains("only https://"));
    assert!(fails("https://areas.example.org/x", net()).contains("did not allow"));
    let mut none = net();
    none.network = None;
    assert!(fails("https://areas.example.org/x", none).contains("no network here"));
    // Allowed: an error status is an answer like any other.
    let host = run("https://areas.example.org/x", net().yes()).unwrap();
    assert_eq!(host.log.borrow().as_slice(), ["404 false"]);
    // A check reaches none (it fails as a finding).
    let host = Rc::new(net().yes());
    let e = run_check(&plugin, "c", Input::new(m.clone(), None), host).unwrap_err();
    assert!(e.to_string().contains("is for commands and wizards"), "{e}");
}

/// `mg.json`: decoded and encoded again as it was, 64-bit numbers, empty
/// arrays and nulls kept; and a GFF's JSON read and written whole.
#[test]
fn mg_json_is_lossless() {
    let code = r#"
        local t = mg.json.decode('{"a":[],"b":{},"big":18446744073709551615,"n":null,"x":[1,2.5,"s"]}')
        assert(tostring(t.big) == "18446744073709551615")
        assert(t.n == mg.json.null)
        local again = mg.json.encode(t)
        assert(again == '{"a":[],"b":{},"big":18446744073709551615,"n":null,"x":[1,2.5,"s"]}', again)
        assert(mg.json.encode({ raw = mg.json.raw(' [1, 2] ') }) == '{"raw":[1, 2]}')
        assert(not pcall(mg.json.raw, "{"))
        assert(not pcall(mg.json.encode, 0/0))
        local text = ctx.module:json("tavern.git")
        ctx.edit:write_json("copy.git", text)
        ctx.edit:write_json("copy2.git", mg.json.decode(text))
        return #text
    "#;
    let m = module();
    let host = Rc::new(NetHost::new(&Arc::new(Stub::default()), &Arc::new(SignIns::in_memory())));
    let out = run_console(code, Input::new(m.clone(), None), host).unwrap();
    let mut ws = Workspace::new(m.clone());
    ws.apply(Command::new("x", out.edits)).unwrap();
    ws.flush().unwrap();
    for name in ["copy.git", "copy2.git"] {
        assert_eq!(ws.module.get(&key(name)), m.get(&key("tavern.git")), "{name}");
    }
}

/// The plugins the fork bundles (`plugins/`) pass their own tests, which
/// run against stubs of their services (`[[test.reply]]`).
#[test]
fn the_bundled_plugins_pass_their_own_tests() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    let found = mg_plugin::folders(&root);
    assert!(!found.is_empty(), "no plugins in {}", root.display());
    for dir in found {
        let plugin = Plugin::load(&dir).unwrap();
        let faults = mg_plugin::test_faults(&plugin);
        assert!(faults.is_empty(), "{faults:?}");
        let results = mg_plugin::run_tests(&plugin, None, &|_, _| Ok(None));
        assert!(results.len() > 20, "{} tests", results.len());
        let failed: Vec<_> = results.iter().filter(|r| !r.passed()).collect();
        assert!(failed.is_empty(), "{failed:#?}");
    }
}
