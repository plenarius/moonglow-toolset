//! The network a plugin reaches (Layonara API 1): HTTPS to the hosts its
//! manifest declares (`[network]`), once the user allows it, from a
//! command or a wizard only. The host hands the job its [`Network`]
//! ([`crate::Host::network`]); without one there is none.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use mlua::{Lua, Table, Value as LuaValue};

use crate::runtime::{Shared, fail};
use crate::signin::SignIns;
use crate::{Answer, Question, SigninDecl};

/// The longest a request may take, all told.
pub const REQUEST_TIME: Duration = Duration::from_secs(30);
/// The largest body a request sends or a response brings, in bytes.
pub const MAX_BODY: usize = 64 << 20;
/// The most requests one job may make.
pub const MAX_REQUESTS: usize = 200;

/// A request as the host sends it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// What came back: redirects are answers too (none is followed).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    /// Each header's name in lower case, and its value.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    /// A header's value, by its name in any case.
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers.iter().find(|(n, _)| *n == name).map(|(_, v)| v.as_str())
    }
}

/// Where a streamed answer goes (`ctx.hak:download`, Layonara API 1): its
/// status and headers, then its body as it arrives.
pub trait Sink {
    fn head(&mut self, status: u16, headers: &[(String, String)]) -> Result<(), String>;
    fn bytes(&mut self, chunk: &[u8]) -> Result<(), String>;
}

/// What sends the requests: HTTPS with ureq in the window and `mg`
/// ([`Http`]), a stub in tests.
pub trait Transport: Send + Sync {
    /// Sends it and waits for the answer; `Err`: no answer (why).
    fn send(&self, request: &Request) -> Result<Response, String>;

    /// Sends it and hands the answer to `sink` as it arrives, with no
    /// limit on its size or time but [`STREAM_IDLE`] between bytes. A
    /// stub's whole answer by default.
    fn stream(&self, request: &Request, sink: &mut dyn Sink) -> Result<(), String> {
        let r = self.send(request)?;
        sink.head(r.status, &r.headers)?;
        sink.bytes(&r.body)
    }
}

/// The longest a streamed answer may go without a byte.
pub const STREAM_IDLE: Duration = Duration::from_secs(60);

/// The network as a host offers it to the jobs it runs.
#[derive(Clone)]
pub struct Network {
    pub transport: Arc<dyn Transport>,
    /// The sign-ins' tokens: the keychain, and what is held in memory.
    pub sign_ins: Arc<SignIns>,
    /// Plain `http://` to this computer (`localhost`, `127.0.0.1`), for
    /// any plugin and without asking: `mg --allow-network-insecure`, tests.
    pub local_http: bool,
}

impl std::fmt::Debug for Network {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Network {{ local_http: {} }}", self.local_http)
    }
}

impl Network {
    pub fn new(transport: Arc<dyn Transport>, sign_ins: Arc<SignIns>) -> Network {
        Network { transport, sign_ins, local_http: false }
    }
}

/// A URL as far as the checks need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    /// `https://`, else `http://`.
    pub secure: bool,
    /// Its host in lower case.
    pub host: String,
    pub port: Option<u16>,
    /// The host is this computer (`localhost`, `127.0.0.1`).
    pub local: bool,
}

impl Url {
    /// Reads an `https://` or `http://` URL: no user in it, no IPv6
    /// literal, no spaces or control characters.
    pub fn parse(text: &str) -> Result<Url, String> {
        if text.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err("a URL has no spaces or control characters".into());
        }
        let lower = text.get(..8).unwrap_or(text).to_ascii_lowercase();
        let (secure, rest) = if lower.starts_with("https://") {
            (true, &text[8..])
        } else if lower.starts_with("http://") {
            (false, &text[7..])
        } else {
            return Err("it is not an https:// URL".into());
        };
        let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
        if authority.contains('@') || authority.contains('[') {
            return Err("a URL with a user name or an IPv6 address is not taken".into());
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => (h, Some(p.parse::<u16>().map_err(|_| format!("port {p:?}"))?)),
            None => (authority, None),
        };
        if host.is_empty() {
            return Err("it names no host".into());
        }
        let host = host.to_ascii_lowercase();
        let local = host == "localhost" || host == "127.0.0.1";
        Ok(Url { secure, host, port, local })
    }
}

/// What a job holds of the network: its plugin's hosts and sign-ins,
/// whether it may reach them at all, and what it has sent.
#[derive(Debug, Default)]
pub(crate) struct NetJob {
    pub(crate) plugin_id: String,
    pub(crate) plugin_name: String,
    pub(crate) hosts: Vec<String>,
    pub(crate) signins: Vec<SigninDecl>,
    /// It may (a command's or a wizard's job); else why not.
    pub(crate) open: Cell<bool>,
    pub(crate) allowed: RefCell<BTreeSet<String>>,
    pub(crate) sent: Cell<usize>,
}

impl NetJob {
    pub(crate) fn of(plugin: &crate::Plugin) -> NetJob {
        let m = &plugin.manifest;
        NetJob {
            plugin_id: m.id.clone(),
            plugin_name: m.name.clone(),
            hosts: m.network.clone(),
            signins: m.signins.clone(),
            ..NetJob::default()
        }
    }

    /// Every host the plugin may reach, its sign-ins' issuers last.
    pub(crate) fn all_hosts(&self) -> Vec<String> {
        let mut all = self.hosts.clone();
        for s in &self.signins {
            let h = s.host();
            if !all.contains(&h) {
                all.push(h);
            }
        }
        all
    }

    pub(crate) fn signin(&self, id: &str) -> mlua::Result<&SigninDecl> {
        match self.signins.iter().find(|s| s.id == id) {
            Some(s) => Ok(s),
            None => {
                let ids: Vec<&str> = self.signins.iter().map(|s| s.id.as_str()).collect();
                fail(format!(
                    "the manifest declares no sign-in {id:?} ([signin]; it has: {})",
                    if ids.is_empty() { "none".to_string() } else { ids.join(", ") }
                ))
            }
        }
    }
}

/// The network a job may use now: its host's, in a job that may reach it.
pub(crate) fn network(sh: &Shared, what: &str) -> mlua::Result<Network> {
    if !sh.net.open.get() {
        return fail(format!(
            "{what} is for commands and wizards: a check, an event, a panel's view, an \
             exporter and an importer do not reach the network"
        ));
    }
    match sh.host.network() {
        Some(n) => Ok(n),
        None => fail(format!(
            "{what}: there is no network here (in mg, --allow-network HOST names each host the \
             plugin may reach)"
        )),
    }
}

/// Checks that the job may send to `url`: HTTPS (or this computer's
/// plain HTTP, where the host allows it), to one of `hosts`, which the
/// user allowed (asked once, for all the plugin's hosts not yet allowed).
pub(crate) fn reach(sh: &Shared, net: &Network, url: &str, hosts: &[String]) -> mlua::Result<()> {
    let u = Url::parse(url).or_else(|e| fail(format!("{url:?}: {e}")))?;
    if u.local && net.local_http {
        return Ok(());
    }
    if !u.secure {
        return fail(format!("{url:?}: only https:// is reached"));
    }
    if !hosts.contains(&u.host) {
        return fail(format!(
            "{url:?}: {} is not a host the manifest declares ([network] hosts: {})",
            u.host,
            if sh.net.hosts.is_empty() { "none".to_string() } else { sh.net.hosts.join(", ") }
        ));
    }
    if sh.net.allowed.borrow().contains(&u.host) {
        return Ok(());
    }
    let asked: Vec<String> =
        (sh.net.all_hosts().into_iter()).filter(|h| !sh.net.allowed.borrow().contains(h)).collect();
    let question = Question::Network { plugin: sh.net.plugin_name.clone(), hosts: asked.clone() };
    if sh.host.ask(&question) != Some(Answer::Yes) {
        return fail(format!("you did not allow {} to reach {}", sh.net.plugin_name, u.host));
    }
    sh.net.allowed.borrow_mut().extend(asked);
    Ok(())
}

/// Sends a request the job may make, as one of its [`MAX_REQUESTS`],
/// on a thread of its own: canceling the job stops waiting for it.
pub(crate) fn send(sh: &Shared, net: &Network, request: Request) -> mlua::Result<Response> {
    let n = sh.net.sent.get() + 1;
    if n > MAX_REQUESTS {
        return fail(format!("a job makes at most {MAX_REQUESTS} requests"));
    }
    sh.net.sent.set(n);
    if request.body.len() > MAX_BODY {
        return fail(format!("a request's body is at most {} MB", MAX_BODY >> 20));
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let transport = net.transport.clone();
    std::thread::spawn(move || {
        let _ = tx.send(transport.send(&request));
    });
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Ok(r)) if r.body.len() > MAX_BODY => {
                return fail(format!(
                    "the answer is past the {} MB a response may be",
                    MAX_BODY >> 20
                ));
            }
            Ok(r) => return r.or_else(fail),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if sh.stopped() => {
                return fail("canceled");
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return fail("the request failed");
            }
        }
    }
}

/// Headers the host sets itself, which a plugin's request does not.
const OWN_HEADERS: [&str; 6] =
    ["host", "content-length", "transfer-encoding", "connection", "upgrade", "te"];

fn header_ok(name: &str, value: &str) -> Result<(), String> {
    let token = |c: char| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c);
    if name.is_empty() || !name.chars().all(token) {
        return Err(format!("{name:?} is not a header's name"));
    }
    if value.chars().any(|c| c == '\r' || c == '\n' || c == '\0') {
        return Err(format!("the header {name}'s value has a line break"));
    }
    if OWN_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
        return Err(format!("the header {name} is Moonglow's to set"));
    }
    Ok(())
}

/// `ctx.net`: `request{ method, url, headers, body, json, signin }`.
pub(crate) fn net_api(lua: &Lua, sh: &Rc<Shared>) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let s = sh.clone();
    t.set(
        "request",
        lua.create_function(move |lua, (_, r): (LuaValue, Table)| request(lua, &s, r))?,
    )?;
    Ok(t)
}

fn request(lua: &Lua, sh: &Rc<Shared>, r: Table) -> mlua::Result<Table> {
    let net = network(sh, "ctx.net:request")?;
    let url: String = match r.get::<Option<String>>("url")? {
        Some(u) => u,
        None => return fail("ctx.net:request{ url = \"https://…\" }: its url"),
    };
    let method = r.get::<Option<String>>("method")?.unwrap_or_else(|| "GET".into());
    let method = method.to_ascii_uppercase();
    if !["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"].contains(&method.as_str()) {
        return fail(format!("{method:?} is not a method (GET, POST, PUT, PATCH, DELETE, HEAD)"));
    }
    let mut headers = Vec::new();
    if let Some(given) = r.get::<Option<Table>>("headers")? {
        for pair in given.pairs::<String, String>() {
            let (name, value) = pair?;
            header_ok(&name, &value).or_else(fail)?;
            headers.push((name, value));
        }
    }
    headers.sort();
    let has = |headers: &[(String, String)], n: &str| {
        headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(n))
    };
    let mut body = match r.get::<Option<mlua::LuaString>>("body")? {
        Some(b) => b.as_bytes().to_vec(),
        None => Vec::new(),
    };
    let json: LuaValue = r.get("json")?;
    if !json.is_nil() {
        if !body.is_empty() {
            return fail("ctx.net:request: a body or json, not both");
        }
        body = crate::json::encode(&json, false)?.into_bytes();
        if !has(&headers, "content-type") {
            headers.push(("Content-Type".into(), "application/json".into()));
        }
    }
    let signin = r.get::<Option<String>>("signin")?;
    if signin.is_some() && has(&headers, "authorization") {
        return fail("ctx.net:request: a sign-in's token is the Authorization header");
    }
    reach(sh, &net, &url, &sh.net.hosts)?;
    let mut req = Request { method, url, headers, body };
    let response = match &signin {
        None => send(sh, &net, req)?,
        Some(id) => {
            let decl = sh.net.signin(id)?.clone();
            let token = crate::signin::bearer(sh, &net, &decl, false)?;
            let with = |req: &Request, token: &str| {
                let mut r = req.clone();
                r.headers.push(("Authorization".into(), format!("Bearer {token}")));
                r
            };
            let first = send(sh, &net, with(&req, &token))?;
            // (Its token refused: refreshed once, and sent again.)
            if first.status == 401 {
                let token = crate::signin::bearer(sh, &net, &decl, true)?;
                req = with(&req, &token);
                send(sh, &net, req)?
            } else {
                first
            }
        }
    };
    let out = lua.create_table()?;
    out.set("status", response.status)?;
    let headers = lua.create_table()?;
    let mut seen = BTreeMap::<String, String>::new();
    for (n, v) in &response.headers {
        seen.entry(n.to_ascii_lowercase())
            .and_modify(|e| {
                e.push_str(", ");
                e.push_str(v);
            })
            .or_insert_with(|| v.clone());
    }
    for (n, v) in seen {
        headers.set(n, v)?;
    }
    out.set("headers", headers)?;
    out.set("body", lua.create_string(&response.body)?)?;
    out.set("ok", (200..300).contains(&response.status))?;
    Ok(out)
}

/// `application/x-www-form-urlencoded`: each pair, its parts escaped.
pub(crate) fn form(pairs: &[(&str, &str)]) -> Vec<u8> {
    let esc = |s: &str| {
        let mut out = String::new();
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    out.push(b as char)
                }
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    };
    let parts: Vec<String> = pairs.iter().map(|(k, v)| format!("{}={}", esc(k), esc(v))).collect();
    parts.join("&").into_bytes()
}

#[cfg(feature = "net")]
pub use http::Http;

#[cfg(feature = "net")]
mod http {
    use super::*;

    /// Requests over HTTPS with ureq (rustls, its own certificates; a
    /// proxy from the environment): no redirect followed, an error status
    /// an answer like any other.
    #[derive(Debug)]
    pub struct Http {
        agent: ureq::Agent,
        /// For downloads: no time limit while bytes arrive.
        streams: ureq::Agent,
    }

    impl Http {
        /// HTTPS only, but (`local_http`) plain HTTP to this computer.
        pub fn new(local_http: bool) -> Http {
            let agent = ureq::Agent::config_builder()
                .https_only(!local_http)
                .user_agent(format!("Moonglow/{}", mg_core::release::version()))
                .max_redirects(0)
                .http_status_as_error(false)
                .timeout_global(Some(REQUEST_TIME))
                .build()
                .into();
            let streams = ureq::Agent::config_builder()
                .https_only(!local_http)
                .user_agent(format!("Moonglow/{}", mg_core::release::version()))
                .max_redirects(0)
                .http_status_as_error(false)
                .timeout_connect(Some(REQUEST_TIME))
                .timeout_recv_response(Some(STREAM_IDLE))
                .timeout_per_read(Some(STREAM_IDLE))
                .build()
                .into();
            Http { agent, streams }
        }
    }

    fn reachable(url: &str) -> Result<(), String> {
        if !url.to_ascii_lowercase().starts_with("https://")
            && !Url::parse(url).is_ok_and(|u| u.local)
        {
            return Err("only https:// is reached".into());
        }
        Ok(())
    }

    fn built(request: &Request) -> Result<ureq::http::Request<Vec<u8>>, String> {
        let mut b = ureq::http::Request::builder()
            .method(request.method.as_str())
            .uri(request.url.as_str());
        for (n, v) in &request.headers {
            b = b.header(n.as_str(), v.as_str());
        }
        b.body(request.body.clone()).map_err(|e| e.to_string())
    }

    fn headers_of(res: &ureq::http::Response<ureq::Body>) -> Vec<(String, String)> {
        (res.headers().iter())
            .map(|(n, v)| (n.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into()))
            .collect()
    }

    impl Transport for Http {
        fn send(&self, request: &Request) -> Result<Response, String> {
            reachable(&request.url)?;
            let mut res = self.agent.run(built(request)?).map_err(why)?;
            let status = res.status().as_u16();
            let headers = headers_of(&res);
            let body = (res.body_mut().with_config().limit(MAX_BODY as u64 + 1).read_to_vec())
                .map_err(why)?;
            Ok(Response { status, headers, body })
        }

        fn stream(&self, request: &Request, sink: &mut dyn Sink) -> Result<(), String> {
            use std::io::Read;
            reachable(&request.url)?;
            let mut res = self.streams.run(built(request)?).map_err(why_stream)?;
            sink.head(res.status().as_u16(), &headers_of(&res))?;
            let mut body = res.body_mut().as_reader();
            let mut buf = vec![0u8; 256 << 10];
            loop {
                let n = match body.read(&mut buf) {
                    Ok(0) => return Ok(()),
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e.to_string()),
                };
                sink.bytes(&buf[..n])?;
            }
        }
    }

    fn why_stream(e: ureq::Error) -> String {
        match e {
            ureq::Error::Timeout(_) => format!("no answer in {} seconds", STREAM_IDLE.as_secs()),
            other => why(other),
        }
    }

    fn why(e: ureq::Error) -> String {
        match e {
            ureq::Error::HostNotFound => "the host was not found (not connected?)".into(),
            ureq::Error::Timeout(_) => {
                format!("no answer in {} seconds", REQUEST_TIME.as_secs())
            }
            ureq::Error::BodyExceedsLimit(_) => {
                format!("the answer is past the {} MB a response may be", MAX_BODY >> 20)
            }
            other => other.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_is_read_as_far_as_its_host() {
        let u = Url::parse("https://Builders.Layonara.com/api/v1/builder/me?x=1").unwrap();
        assert_eq!(
            (u.secure, u.host.as_str(), u.port, u.local),
            (true, "builders.layonara.com", None, false)
        );
        let u = Url::parse("http://127.0.0.1:8100/x").unwrap();
        assert_eq!((u.secure, u.port, u.local), (false, Some(8100), true));
        assert!(Url::parse("https://evil.org@builders.layonara.com/").is_err());
        assert!(Url::parse("ftp://x.org/").is_err());
        assert!(Url::parse("https:///x").is_err());
        assert!(Url::parse("https://a.org/ x").is_err());
        assert!(Url::parse("https://[::1]/").is_err());
        assert!(Url::parse("https://a.org:99999/").is_err());
        assert_eq!(Url::parse("https://a.org#x").unwrap().host, "a.org");
    }

    #[test]
    fn a_form_is_escaped() {
        let f =
            form(&[("grant_type", "urn:ietf:params:oauth:grant-type:device_code"), ("s", "a b")]);
        assert_eq!(
            String::from_utf8(f).unwrap(),
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&s=a%20b"
        );
    }

    #[test]
    fn a_plugin_s_header_is_checked() {
        assert!(header_ok("X-Trace", "1").is_ok());
        assert!(header_ok("Host", "evil.org").is_err());
        assert!(header_ok("X", "a\r\nB: c").is_err());
        assert!(header_ok("bad name", "x").is_err());
    }

    /// The window's transport: an error status and a redirect are answers
    /// (the redirect not followed), plain HTTP only to this computer.
    #[cfg(feature = "net")]
    #[test]
    fn http_answers_without_following_redirects() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for (stream, reply) in listener.incoming().zip([
                "HTTP/1.1 302 Found\r\nLocation: https://evil.example.org/\r\nContent-Length: 0\r\n\r\n",
                "HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\nX-A: 1\r\n\r\nno",
            ]) {
                let mut stream = stream.unwrap();
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                stream.write_all(reply.as_bytes()).unwrap();
            }
        });
        let http = Http::new(true);
        let get = |path: &str| Request {
            method: "GET".into(),
            url: format!("http://127.0.0.1:{port}{path}"),
            headers: Vec::new(),
            body: Vec::new(),
        };
        let r = http.send(&get("/a")).unwrap();
        assert_eq!((r.status, r.header("location")), (302, Some("https://evil.example.org/")));
        let r = http.send(&get("/b")).unwrap();
        assert_eq!(
            (r.status, r.body.as_slice(), r.header("X-A")),
            (404, b"no".as_slice(), Some("1"))
        );
        let mut far = get("/");
        far.url = "http://example.org/".into();
        assert!(Http::new(true).send(&far).is_err(), "plain HTTP elsewhere");
    }

    /// The window's transport streams an answer to its sink in pieces,
    /// with its status and headers first.
    #[cfg(feature = "net")]
    #[test]
    fn http_streams_an_answer_to_its_sink() {
        use std::io::{Read, Write};
        let body: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
        let sent = body.clone();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap();
            assert!(String::from_utf8_lossy(&buf[..n]).contains("range: bytes=100-"));
            let head = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes \
                 100-{}/{}\r\n\r\n",
                sent.len() - 100,
                sent.len() - 1,
                sent.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&sent[100..]).unwrap();
        });
        #[derive(Default)]
        struct Kept {
            status: u16,
            range: Option<String>,
            body: Vec<u8>,
            pieces: usize,
        }
        impl Sink for Kept {
            fn head(&mut self, status: u16, headers: &[(String, String)]) -> Result<(), String> {
                self.status = status;
                self.range =
                    headers.iter().find(|(k, _)| k == "content-range").map(|h| h.1.clone());
                Ok(())
            }
            fn bytes(&mut self, chunk: &[u8]) -> Result<(), String> {
                self.body.extend_from_slice(chunk);
                self.pieces += 1;
                Ok(())
            }
        }
        let mut kept = Kept::default();
        let request = Request {
            method: "GET".into(),
            url: format!("http://127.0.0.1:{port}/hak"),
            headers: vec![("Range".into(), "bytes=100-".into())],
            body: Vec::new(),
        };
        Http::new(true).stream(&request, &mut kept).unwrap();
        assert_eq!((kept.status, kept.range.as_deref()), (206, Some("bytes 100-699999/700000")));
        assert_eq!(kept.body, body[100..]);
        assert!(kept.pieces > 1, "in pieces, not whole");
    }
}
