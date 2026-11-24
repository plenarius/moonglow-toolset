//! Signing in (API 0.10): OAuth 2.0's device authorization (RFC 8628)
//! against an OpenID issuer a plugin's manifest declares (`[signin]`),
//! run by the host. The refresh token is kept in the system's keychain
//! (or, where there is none, in memory for the session); access tokens
//! stay in memory. A plugin's code never sees either: `ctx.net:request`
//! puts the access token into its request's Authorization header.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use mlua::{Lua, Table, Value as LuaValue};
use serde_json::Value as Json;

use crate::net::{Network, Request, Response, Url, form, network, reach, send};
use crate::runtime::{Shared, fail};
use crate::{Question, SigninDecl};

/// Where secrets are kept between runs: the system's keychain
/// ([`OsKeychain`]), or memory ([`MemoryKeychain`]: tests, and where
/// there is no keychain).
pub trait Keychain: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>, String>;
    /// `Err`: not kept (no keychain here, or it refused).
    fn set(&self, account: &str, secret: &str) -> Result<(), String>;
    fn delete(&self, account: &str) -> Result<(), String>;
    /// The system's own keychain (tests check they never reach it).
    fn is_system(&self) -> bool {
        false
    }
}

/// Secrets held in memory alone.
#[derive(Debug, Default)]
pub struct MemoryKeychain(Mutex<HashMap<String, String>>);

impl Keychain for MemoryKeychain {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        Ok(self.0.lock().unwrap_or_else(|e| e.into_inner()).get(account).cloned())
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), String> {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.insert(account.to_string(), secret.to_string());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(account);
        Ok(())
    }
}

/// The keychain's service name entries are kept under.
pub const SERVICE: &str = "moonglow";

#[cfg(feature = "net")]
pub use os::OsKeychain;

#[cfg(feature = "net")]
mod os {
    /// The system's keychain (the `keyring` crate): Windows' Credential
    /// Manager, the macOS Keychain, the Secret Service on Linux.
    #[derive(Debug, Default)]
    pub struct OsKeychain;

    fn entry(account: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(super::SERVICE, account).map_err(|e| e.to_string())
    }

    impl super::Keychain for OsKeychain {
        fn get(&self, account: &str) -> Result<Option<String>, String> {
            match entry(account)?.get_password() {
                Ok(s) => Ok(Some(s)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(e.to_string()),
            }
        }

        fn set(&self, account: &str, secret: &str) -> Result<(), String> {
            entry(account)?.set_password(secret).map_err(|e| e.to_string())
        }

        fn delete(&self, account: &str) -> Result<(), String> {
            match entry(account)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(e.to_string()),
            }
        }

        fn is_system(&self) -> bool {
            true
        }
    }
}

/// What an issuer's discovery document says of where to go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    pub device: String,
    pub token: String,
    pub revoke: Option<String>,
}

/// An access token in memory, and when it runs out.
#[derive(Debug, Clone)]
struct Access {
    token: String,
    until: Instant,
}

/// A sign-in as this run holds it.
#[derive(Debug, Clone, Default)]
struct Held {
    access: Option<Access>,
    /// The refresh token, where the keychain could not keep it.
    refresh: Option<String>,
    name: String,
}

/// The sign-ins of every plugin, for one run of Moonglow: the keychain,
/// and what is held in memory (access tokens, discovery documents).
pub struct SignIns {
    keychain: Box<dyn Keychain>,
    held: Mutex<HashMap<String, Held>>,
    endpoints: Mutex<HashMap<String, Endpoints>>,
}

impl std::fmt::Debug for SignIns {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SignIns")
    }
}

/// What is kept in the keychain: the refresh token and whose it is.
#[derive(serde::Serialize, serde::Deserialize)]
struct Kept {
    refresh_token: String,
    #[serde(default)]
    name: String,
}

/// Whether a plugin's sign-in is signed in, and as whom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub signed_in: bool,
    pub name: String,
    /// It is kept in the keychain, for the next run (else in memory).
    pub kept: bool,
}

/// The keychain's account of a plugin's sign-in.
pub fn account(plugin_id: &str, decl: &SigninDecl) -> String {
    format!("{plugin_id}/{}/{}", decl.id, decl.issuer)
}

impl SignIns {
    pub fn new(keychain: Box<dyn Keychain>) -> SignIns {
        SignIns { keychain, held: Mutex::default(), endpoints: Mutex::default() }
    }

    /// Whether its secrets go to the system's keychain.
    pub fn in_system_keychain(&self) -> bool {
        self.keychain.is_system()
    }

    /// Held in memory alone: tests.
    pub fn in_memory() -> SignIns {
        SignIns::new(Box::new(MemoryKeychain::default()))
    }

    fn held(&self) -> std::sync::MutexGuard<'_, HashMap<String, Held>> {
        self.held.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn kept(&self, account: &str) -> Option<Kept> {
        let text = self.keychain.get(account).ok().flatten()?;
        serde_json::from_str(&text).ok()
    }

    /// Signs an account in as `name` with an access token good for an
    /// hour and no refresh token: a plugin's tests (`signed_in`).
    pub fn hold(&self, account: &str, name: &str, access_token: &str) {
        let until = Instant::now() + Duration::from_secs(3600);
        let access = Some(Access { token: access_token.to_string(), until });
        self.held().insert(account.to_string(), Held { access, refresh: None, name: name.into() });
    }

    /// Whether it is signed in, without the network.
    pub fn status(&self, account: &str) -> Status {
        if let Some(k) = self.kept(account) {
            let name = self.held().get(account).map(|h| h.name.clone()).unwrap_or(k.name);
            return Status { signed_in: true, name, kept: true };
        }
        match self.held().get(account) {
            Some(h) if h.refresh.is_some() || h.access.is_some() => {
                Status { signed_in: true, name: h.name.clone(), kept: false }
            }
            _ => Status { signed_in: false, name: String::new(), kept: false },
        }
    }

    /// The refresh token, wherever it is kept.
    fn refresh_token(&self, account: &str) -> Option<String> {
        self.held()
            .get(account)
            .and_then(|h| h.refresh.clone())
            .or_else(|| self.kept(account).map(|k| k.refresh_token))
    }

    /// Keeps what a token endpoint answered: whether the keychain kept
    /// the refresh token (else it is held for the session).
    fn keep(&self, account: &str, tokens: &Tokens) -> bool {
        let refresh = tokens.refresh.clone().or_else(|| self.refresh_token(account));
        let name = match &tokens.name {
            Some(n) => n.clone(),
            None => self.status(account).name,
        };
        let mut kept = false;
        if let Some(r) = &refresh {
            let text = serde_json::json!({ "refresh_token": r, "name": name }).to_string();
            kept = self.keychain.set(account, &text).is_ok();
        }
        let until = Instant::now() + Duration::from_secs(tokens.expires_in.clamp(1, 86_400));
        let mut held = self.held();
        let h = held.entry(account.to_string()).or_default();
        h.access = Some(Access { token: tokens.access.clone(), until });
        h.refresh = if kept { None } else { refresh };
        h.name = name;
        kept
    }

    /// Forgets a sign-in here (the keychain's entry, what is in memory),
    /// without telling the issuer: what [`sign_out`] does after revoking.
    pub fn forget(&self, account: &str) -> Result<(), String> {
        self.held().remove(account);
        self.keychain.delete(account)
    }

    /// The access token in memory, still good for a while.
    fn access(&self, account: &str) -> Option<String> {
        let held = self.held();
        let a = held.get(account)?.access.as_ref()?;
        (a.until > Instant::now() + Duration::from_secs(30)).then(|| a.token.clone())
    }

    fn drop_access(&self, account: &str) {
        if let Some(h) = self.held().get_mut(account) {
            h.access = None;
        }
    }
}

/// What a token endpoint gave.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tokens {
    access: String,
    refresh: Option<String>,
    expires_in: u64,
    /// The ID token's `preferred_username` (else `name`, `sub`).
    name: Option<String>,
}

/// The claims of a JWT's payload (not checked: only the name is read).
fn claims(jwt: &str) -> Option<Json> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn tokens_of(json: &Json) -> Result<Tokens, String> {
    let access = json["access_token"].as_str().ok_or("the answer has no access_token")?;
    let name = json["id_token"].as_str().and_then(claims).and_then(|c| {
        ["preferred_username", "name", "sub"]
            .iter()
            .find_map(|k| c[k].as_str().filter(|s| !s.is_empty()).map(str::to_string))
    });
    Ok(Tokens {
        access: access.to_string(),
        refresh: json["refresh_token"].as_str().map(str::to_string),
        expires_in: json["expires_in"].as_u64().unwrap_or(300),
        name,
    })
}

fn json_of(r: &Response) -> Json {
    serde_json::from_slice(&r.body).unwrap_or(Json::Null)
}

/// An OAuth error's code and what it says.
fn oauth_error(r: &Response) -> (String, String) {
    let j = json_of(r);
    let code = j["error"].as_str().unwrap_or_default().to_string();
    let text = j["error_description"].as_str().unwrap_or_default().to_string();
    (code, text)
}

fn post_form(url: &str, pairs: &[(&str, &str)]) -> Request {
    Request {
        method: "POST".into(),
        url: url.to_string(),
        headers: vec![
            ("Accept".into(), "application/json".into()),
            ("Content-Type".into(), "application/x-www-form-urlencoded".into()),
        ],
        body: form(pairs),
    }
}

/// Where the issuer's endpoints are (its discovery document, read once a
/// run): each on the issuer's own host.
fn endpoints(sh: &Shared, net: &Network, decl: &SigninDecl) -> mlua::Result<Endpoints> {
    let issuer = decl.issuer.trim_end_matches('/');
    if let Some(e) = net.sign_ins.endpoints.lock().unwrap_or_else(|e| e.into_inner()).get(issuer) {
        return Ok(e.clone());
    }
    let host = decl.host();
    let url = format!("{issuer}/.well-known/openid-configuration");
    reach(sh, net, &url, std::slice::from_ref(&host))?;
    let get = Request {
        method: "GET".into(),
        url: url.clone(),
        headers: vec![("Accept".into(), "application/json".into())],
        body: Vec::new(),
    };
    let r = send(sh, net, get)?;
    if r.status != 200 {
        return fail(format!("{url}: the issuer answered {}", r.status));
    }
    let doc = json_of(&r);
    if doc["issuer"].as_str().map(|i| i.trim_end_matches('/')) != Some(issuer) {
        return fail(format!("{url}: the document is not the issuer {issuer:?}'s"));
    }
    let on_host = |key: &str| -> mlua::Result<Option<String>> {
        let Some(at) = doc[key].as_str() else { return Ok(None) };
        let u = Url::parse(at).or_else(|e| fail(format!("{key}: {e}")))?;
        if u.host != host || !(u.secure || (u.local && net.local_http)) {
            return fail(format!("the issuer's {key} {at:?} is not on https://{host}"));
        }
        Ok(Some(at.to_string()))
    };
    let Some(device) = on_host("device_authorization_endpoint")? else {
        return fail(format!("the issuer {issuer:?} has no device flow (sign-in by a code)"));
    };
    let Some(token) = on_host("token_endpoint")? else {
        return fail(format!("the issuer {issuer:?} has no token endpoint"));
    };
    let e = Endpoints { device, token, revoke: on_host("revocation_endpoint")? };
    let mut all = net.sign_ins.endpoints.lock().unwrap_or_else(|e| e.into_inner());
    all.insert(issuer.to_string(), e.clone());
    Ok(e)
}

/// A device code shown to the user while the host waits for the issuer
/// to say they entered it.
#[derive(Debug, Clone)]
pub struct SignInCode {
    /// The service's host (`auth.example.org`), and the plugin's name.
    pub service: String,
    pub plugin: String,
    /// What to type, and where.
    pub code: String,
    pub uri: String,
    /// The page with the code filled in, if the issuer gives one.
    pub uri_complete: Option<String>,
    pub waiting: Arc<Waiting>,
}

impl PartialEq for SignInCode {
    fn eq(&self, o: &SignInCode) -> bool {
        (&self.service, &self.plugin, &self.code, &self.uri, &self.uri_complete)
            == (&o.service, &o.plugin, &o.code, &o.uri, &o.uri_complete)
    }
}

/// Whether the issuer has answered a sign-in waited for.
#[derive(Debug, Default)]
pub struct Waiting {
    done: AtomicBool,
}

impl Waiting {
    /// The issuer answered (signed in, refused, or the code ran out): the
    /// question is over.
    pub fn done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
}

/// Signs the user in by a device code: the code shown (the host's
/// question), the issuer polled on a thread of its own meanwhile.
pub(crate) fn start(sh: &Shared, net: &Network, decl: &SigninDecl) -> mlua::Result<Status> {
    let e = endpoints(sh, net, decl)?;
    let pairs = [("client_id", decl.client.as_str()), ("scope", decl.scopes.as_str())];
    let r = send(sh, net, post_form(&e.device, &pairs))?;
    if r.status != 200 {
        let (code, text) = oauth_error(&r);
        return fail(format!("the issuer refused the sign-in ({} {code} {text})", r.status));
    }
    let j = json_of(&r);
    let (Some(device), Some(user), Some(uri)) =
        (j["device_code"].as_str(), j["user_code"].as_str(), j["verification_uri"].as_str())
    else {
        return fail("the issuer's answer has no device_code, user_code or verification_uri");
    };
    let interval = j["interval"].as_u64().unwrap_or(5).clamp(1, 60);
    let expires = j["expires_in"].as_u64().unwrap_or(600).clamp(10, 3600);
    let waiting = Arc::new(Waiting::default());
    let stop = Arc::new(AtomicBool::new(false));
    let got: Arc<Mutex<Option<Result<Tokens, String>>>> = Arc::default();
    {
        let (transport, waiting, stop, got) =
            (net.transport.clone(), waiting.clone(), stop.clone(), got.clone());
        let (token_url, client, device) =
            (e.token.clone(), decl.client.clone(), device.to_string());
        std::thread::spawn(move || {
            let r = poll(&*transport, &token_url, &client, &device, interval, expires, &stop);
            *got.lock().unwrap_or_else(|e| e.into_inner()) = r;
            waiting.done.store(true, Ordering::Release);
        });
    }
    let code = SignInCode {
        service: decl.host(),
        plugin: sh.net.plugin_name.clone(),
        code: user.to_string(),
        uri: uri.to_string(),
        uri_complete: j["verification_uri_complete"].as_str().map(str::to_string),
        waiting,
    };
    let _ = sh.host.ask(&Question::SignIn(code));
    stop.store(true, Ordering::Release);
    let account = account(&sh.net.plugin_id, decl);
    match got.lock().unwrap_or_else(|e| e.into_inner()).take() {
        Some(Ok(tokens)) => {
            if !net.sign_ins.keep(&account, &tokens) {
                sh.host.log(
                    crate::Level::Warning,
                    "there is no keychain to keep the sign-in in: sign in again next time",
                );
            }
            Ok(net.sign_ins.status(&account))
        }
        Some(Err(why)) => fail(why),
        None if sh.stopped() => fail("canceled"),
        None => Ok(net.sign_ins.status(&account)),
    }
}

/// Polls the token endpoint at the issuer's pace until it answers, the
/// code runs out, or `stop`: the tokens, why not, or nothing (stopped).
fn poll(
    t: &dyn crate::net::Transport,
    url: &str,
    client: &str,
    device: &str,
    mut interval: u64,
    expires: u64,
    stop: &AtomicBool,
) -> Option<Result<Tokens, String>> {
    let until = Instant::now() + Duration::from_secs(expires);
    let grant = "urn:ietf:params:oauth:grant-type:device_code";
    loop {
        let next = Instant::now() + Duration::from_secs(interval);
        while Instant::now() < next {
            if stop.load(Ordering::Acquire) {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if Instant::now() > until {
            return Some(Err("the code ran out before it was entered: sign in again".into()));
        }
        let pairs = [("grant_type", grant), ("device_code", device), ("client_id", client)];
        let r = match t.send(&post_form(url, &pairs)) {
            Ok(r) => r,
            Err(e) => return Some(Err(e)),
        };
        if r.status == 200 {
            return Some(tokens_of(&json_of(&r)));
        }
        match oauth_error(&r) {
            (c, _) if c == "authorization_pending" => {}
            (c, _) if c == "slow_down" => interval += 5,
            (c, _) if c == "access_denied" => return Some(Err("the sign-in was refused".into())),
            (c, _) if c == "expired_token" => {
                return Some(Err("the code ran out before it was entered: sign in again".into()));
            }
            (c, text) => {
                return Some(Err(format!("the issuer answered {} {c} {text}", r.status)));
            }
        }
    }
}

/// An access token for a request: the one in memory, else (or `fresh`)
/// one the refresh token gets. Its refusal signs the user out.
pub(crate) fn bearer(
    sh: &Shared,
    net: &Network,
    decl: &SigninDecl,
    fresh: bool,
) -> mlua::Result<String> {
    let account = account(&sh.net.plugin_id, decl);
    if fresh {
        net.sign_ins.drop_access(&account);
    } else if let Some(t) = net.sign_ins.access(&account) {
        return Ok(t);
    }
    let Some(refresh) = net.sign_ins.refresh_token(&account) else {
        return fail(format!("not signed in to {} (ctx.signin:start({:?}))", decl.host(), decl.id));
    };
    let e = endpoints(sh, net, decl)?;
    let pairs = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh.as_str()),
        ("client_id", decl.client.as_str()),
    ];
    let r = send(sh, net, post_form(&e.token, &pairs))?;
    if r.status != 200 {
        let (code, text) = oauth_error(&r);
        if code == "invalid_grant" {
            let _ = net.sign_ins.forget(&account);
            return fail(format!(
                "the sign-in to {} has ended ({text}): sign in again (ctx.signin:start({:?}))",
                decl.host(),
                decl.id
            ));
        }
        return fail(format!("the issuer answered {} {code} {text}", r.status));
    }
    let tokens = tokens_of(&json_of(&r)).or_else(fail)?;
    net.sign_ins.keep(&account, &tokens);
    Ok(tokens.access)
}

/// Signs out: the refresh token revoked at the issuer (if it can be),
/// then forgotten here whatever the issuer said.
pub(crate) fn stop(sh: &Shared, net: &Network, decl: &SigninDecl) -> mlua::Result<()> {
    let account = account(&sh.net.plugin_id, decl);
    if let Some(refresh) = net.sign_ins.refresh_token(&account) {
        let revoked = endpoints(sh, net, decl).and_then(|e| match &e.revoke {
            Some(url) => {
                let pairs = [
                    ("token", refresh.as_str()),
                    ("token_type_hint", "refresh_token"),
                    ("client_id", decl.client.as_str()),
                ];
                send(sh, net, post_form(url, &pairs)).map(|_| ())
            }
            None => Ok(()),
        });
        if let Err(e) = revoked {
            sh.host.log(crate::Level::Warning, &format!("the issuer was not told: {e}"));
        }
    }
    net.sign_ins.forget(&account).or_else(|e| fail(format!("the keychain: {e}")))
}

/// Signs a plugin's sign-in out from the host (the Plugins tab's Sign
/// Out, Forget, removing it), outside any job: revoked at the issuer
/// with what this run discovered of it, and forgotten here.
pub fn sign_out(net: &Network, plugin_id: &str, decl: &SigninDecl) -> Result<(), String> {
    let account = account(plugin_id, decl);
    let issuer = decl.issuer.trim_end_matches('/').to_string();
    let found =
        net.sign_ins.endpoints.lock().unwrap_or_else(|e| e.into_inner()).get(&issuer).cloned();
    let revoke = found.and_then(|e| e.revoke);
    if let (Some(refresh), Some(url)) = (net.sign_ins.refresh_token(&account), revoke) {
        let pairs = [
            ("token", refresh.as_str()),
            ("token_type_hint", "refresh_token"),
            ("client_id", decl.client.as_str()),
        ];
        let _ = net.transport.send(&post_form(&url, &pairs));
    }
    net.sign_ins.forget(&account)
}

fn status_table(lua: &Lua, s: &Status) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("signed_in", s.signed_in)?;
    if !s.name.is_empty() {
        t.set("name", s.name.as_str())?;
    }
    t.set("kept", s.kept)?;
    Ok(t)
}

/// `ctx.signin`: `start(id)`, `status(id)`, `stop(id)`.
pub(crate) fn signin_api(lua: &Lua, sh: &std::rc::Rc<Shared>) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let s = sh.clone();
    t.set(
        "start",
        lua.create_function(move |lua, (_, id): (LuaValue, String)| {
            let net = network(&s, "ctx.signin:start")?;
            let decl = s.net.signin(&id)?.clone();
            let status = start(&s, &net, &decl)?;
            status_table(lua, &status)
        })?,
    )?;
    let s = sh.clone();
    t.set(
        "status",
        lua.create_function(move |lua, (_, id): (LuaValue, String)| {
            let net = network(&s, "ctx.signin:status")?;
            let decl = s.net.signin(&id)?;
            status_table(lua, &net.sign_ins.status(&account(&s.net.plugin_id, decl)))
        })?,
    )?;
    let s = sh.clone();
    t.set(
        "stop",
        lua.create_function(move |_, (_, id): (LuaValue, String)| {
            let net = network(&s, "ctx.signin:stop")?;
            let decl = s.net.signin(&id)?.clone();
            stop(&s, &net, &decl)
        })?,
    )?;
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_answer_is_read_with_its_name() {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"sub":"abc","preferred_username":"alice"}"#);
        let j = serde_json::json!({
            "access_token": "AT", "refresh_token": "RT", "expires_in": 600,
            "id_token": format!("h.{payload}.s"),
        });
        let t = tokens_of(&j).unwrap();
        assert_eq!(
            (t.access.as_str(), t.refresh.as_deref(), t.expires_in),
            ("AT", Some("RT"), 600)
        );
        assert_eq!(t.name.as_deref(), Some("alice"));
        assert!(tokens_of(&serde_json::json!({})).is_err());
    }

    #[test]
    fn what_is_kept_is_the_refresh_token_and_the_name() {
        let s = SignIns::in_memory();
        let t = Tokens {
            access: "AT".into(),
            refresh: Some("RT".into()),
            expires_in: 600,
            name: Some("alice".into()),
        };
        assert!(s.keep("p/x/i", &t));
        let kept = s.keychain.get("p/x/i").unwrap().unwrap();
        assert!(!kept.contains("AT") && kept.contains("RT") && kept.contains("alice"), "{kept}");
        assert_eq!(s.status("p/x/i"), Status { signed_in: true, name: "alice".into(), kept: true });
        assert_eq!(s.access("p/x/i").as_deref(), Some("AT"));
        s.forget("p/x/i").unwrap();
        assert!(!s.status("p/x/i").signed_in);
    }

    /// No keychain: the refresh token is held in memory for the session.
    #[test]
    fn without_a_keychain_it_is_held_for_the_session() {
        struct None_;
        impl Keychain for None_ {
            fn get(&self, _: &str) -> Result<Option<String>, String> {
                Err("no Secret Service".into())
            }
            fn set(&self, _: &str, _: &str) -> Result<(), String> {
                Err("no Secret Service".into())
            }
            fn delete(&self, _: &str) -> Result<(), String> {
                Ok(())
            }
        }
        let s = SignIns::new(Box::new(None_));
        let t =
            Tokens { access: "AT".into(), refresh: Some("RT".into()), expires_in: 600, name: None };
        assert!(!s.keep("a", &t));
        assert_eq!(s.refresh_token("a").as_deref(), Some("RT"));
        assert_eq!(s.status("a"), Status { signed_in: true, name: String::new(), kept: false });
    }
}
