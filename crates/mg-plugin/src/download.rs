//! `ctx.hak:download` (API 0.11, Moonglow's Layonara build): a whole hak
//! streamed from a host the plugin declares into the user's hak folder,
//! beside the hak as `<hak>.hak.part`, never through the plugin's memory;
//! checked by its size and SHA-256, and handed back with the job's haks to
//! be put in the hak's place (the old one kept for Undo). A download cut
//! short goes on from where it stopped the next time.

use std::collections::BTreeSet;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use mg_core::sha256::Sha256;
use mlua::{Lua, Table};

use crate::net::{Network, Request, STREAM_IDLE, Sink, network, reach};
use crate::runtime::{Shared, fail};

/// The largest hak a plugin downloads, in bytes.
pub const MAX_DOWNLOAD: u64 = 512 << 20;

/// The hak folder's files being downloaded into, by this process.
static UNDER_WAY: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// What was asked for: `{ url, signin, sha256, bytes, above_nwsync }`.
struct Wanted {
    url: String,
    signin: Option<String>,
    sha256: [u8; 32],
    bytes: u64,
    above_nwsync: bool,
}

fn wanted(spec: &Table) -> mlua::Result<Wanted> {
    let what = "ctx.hak:download(hak, { url, sha256, bytes })";
    let Some(url) = spec.get::<Option<String>>("url")? else {
        return fail(format!("{what}: its url"));
    };
    let Some(sha) = spec.get::<Option<String>>("sha256")? else {
        return fail(format!("{what}: its sha256"));
    };
    let Some(sha256) = parse_sha(&sha) else {
        return fail(format!("{what}: {sha:?} is not a SHA-256 (64 hex digits)"));
    };
    let bytes = match spec.get::<Option<f64>>("bytes")? {
        Some(n) if n.fract() == 0.0 && n >= 1.0 && n <= MAX_DOWNLOAD as f64 => n as u64,
        Some(n) => {
            return fail(format!(
                "{what}: bytes {n} is not a size from 1 to {} MB",
                MAX_DOWNLOAD >> 20
            ));
        }
        None => return fail(format!("{what}: its bytes")),
    };
    Ok(Wanted {
        url,
        signin: spec.get("signin")?,
        sha256,
        bytes,
        above_nwsync: spec.get::<Option<bool>>("above_nwsync")?.unwrap_or(false),
    })
}

fn parse_sha(text: &str) -> Option<[u8; 32]> {
    let t = text.trim();
    if t.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(t.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

fn hex(sha: &[u8; 32]) -> String {
    sha.iter().map(|b| format!("{b:02x}")).collect()
}

/// The SHA-256 of a file's first `len` bytes, read in pieces; `None`
/// where it can't be read or is shorter.
fn hash_of(path: &Path, len: u64) -> Option<Sha256> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut sha = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut left = len;
    while left > 0 {
        let want = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
        let n = file.read(&mut buf[..want]).ok()?;
        if n == 0 {
            return None;
        }
        sha.update(&buf[..n]);
        left -= n as u64;
    }
    Some(sha)
}

/// Whether the file is these bytes.
fn is(path: &Path, w: &Wanted) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.len() == w.bytes)
        && hash_of(path, w.bytes).is_some_and(|h| h.finish() == w.sha256)
}

fn mb(n: u64) -> String {
    format!("{:.1}", n as f64 / (1u64 << 20) as f64)
}

/// The part file, held by this download alone while it runs.
struct Claim(PathBuf);

impl Claim {
    fn take(path: &Path) -> Option<Claim> {
        let mut busy = UNDER_WAY.lock().unwrap_or_else(PoisonError::into_inner);
        busy.insert(path.to_path_buf()).then(|| Claim(path.to_path_buf()))
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        UNDER_WAY.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.0);
    }
}

/// What the stream's thread and the job share: the bytes the part file
/// has, and whether the thread may still write it.
struct Progress {
    got: AtomicU64,
    open: Mutex<bool>,
}

/// The part file as the answer fills it.
struct Part {
    file: std::fs::File,
    sha: Sha256,
    from: u64,
    total: u64,
    state: Arc<Progress>,
    status: u16,
    /// A refusal's first bytes, for its message.
    said: Vec<u8>,
}

impl Sink for Part {
    fn head(&mut self, status: u16, headers: &[(String, String)]) -> Result<(), String> {
        self.status = status;
        let header = |n: &str| {
            headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.trim())
        };
        match status {
            206 if self.from > 0 => {
                let range = header("content-range").unwrap_or("");
                if !range.starts_with(&format!("bytes {}-", self.from)) {
                    return Err(format!(
                        "the server sent the range {range:?}, not from byte {}",
                        self.from
                    ));
                }
            }
            200..=299 => {
                // (The whole hak: what the part had is written over.)
                self.from = 0;
                self.sha = Sha256::new();
                self.state.got.store(0, Ordering::Relaxed);
                self.file.set_len(0).map_err(|e| e.to_string())?;
                self.file.rewind().map_err(|e| e.to_string())?;
            }
            _ => return Ok(()),
        }
        if let Some(n) = header("content-length").and_then(|v| v.parse::<u64>().ok())
            && n != self.total - self.from
        {
            return Err(format!(
                "the server sends {} MB, not the {} MB the hak has left",
                mb(n),
                mb(self.total - self.from)
            ));
        }
        Ok(())
    }

    fn bytes(&mut self, chunk: &[u8]) -> Result<(), String> {
        if !(200..300).contains(&self.status) {
            let room = 512usize.saturating_sub(self.said.len());
            self.said.extend_from_slice(&chunk[..chunk.len().min(room)]);
            return Ok(());
        }
        let open = self.state.open.lock().unwrap_or_else(PoisonError::into_inner);
        if !*open {
            return Err("canceled".into());
        }
        let got = self.state.got.load(Ordering::Relaxed) + chunk.len() as u64;
        if self.from + got > self.total {
            return Err(format!("the server sends more than the hak's {} MB", mb(self.total)));
        }
        self.file.write_all(chunk).map_err(|e| e.to_string())?;
        self.sha.update(chunk);
        self.state.got.store(got, Ordering::Relaxed);
        Ok(())
    }
}

/// How one request of the download ended.
enum Ended {
    /// The part file has all its bytes: their SHA-256.
    Whole([u8; 32]),
    /// A status that is not a success, and what the server said.
    Refused(u16, String),
}

/// One request: streams the answer into the part file from `from` (its
/// bytes so far hashed into `sha`) on a thread of its own, while the job
/// shows the progress, and stops on Cancel or [`STREAM_IDLE`] without a
/// byte.
fn fetch(
    sh: &Shared,
    net: &Network,
    hak: &str,
    part: &Path,
    request: Request,
    (from, sha): (u64, Sha256),
    w: &Wanted,
) -> mlua::Result<Ended> {
    let n = sh.net.sent.get() + 1;
    if n > crate::net::MAX_REQUESTS {
        return fail(format!("a job makes at most {} requests", crate::net::MAX_REQUESTS));
    }
    sh.net.sent.set(n);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(part)
        .or_else(|e| fail(format!("{}: {e}", part.display())))?;
    file.set_len(from)
        .and_then(|()| file.seek(std::io::SeekFrom::Start(from)).map(|_| ()))
        .or_else(|e| fail(format!("{}: {e}", part.display())))?;
    let state = Arc::new(Progress { got: AtomicU64::new(0), open: Mutex::new(true) });
    let mut sink =
        Part { file, sha, from, total: w.bytes, state: state.clone(), status: 0, said: Vec::new() };
    let (tx, rx) = std::sync::mpsc::channel();
    let transport = net.transport.clone();
    std::thread::spawn(move || {
        let r = transport.stream(&request, &mut sink);
        let _ = sink.file.flush();
        let _ = tx.send(r.map(|()| sink));
    });
    let stop = |why: String| -> mlua::Result<Ended> {
        *state.open.lock().unwrap_or_else(PoisonError::into_inner) = false;
        fail(why)
    };
    let mut last = (0u64, Instant::now());
    let mut shown = u64::MAX;
    loop {
        let got = state.got.load(Ordering::Relaxed);
        if got != last.0 {
            last = (got, Instant::now());
        }
        let done = from + got;
        if done >> 20 != shown {
            shown = done >> 20;
            let note = format!("Downloading {hak}.hak: {} of {} MB", done >> 20, mb(w.bytes));
            sh.host.progress((done >> 10) as usize, (w.bytes >> 10) as usize, &note);
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(sink)) if !(200..300).contains(&sink.status) => {
                let said = String::from_utf8_lossy(&sink.said).trim().to_string();
                return Ok(Ended::Refused(sink.status, said));
            }
            Ok(Ok(sink)) => {
                let have = sink.from + sink.state.got.load(Ordering::Relaxed);
                if have != w.bytes {
                    return fail(format!(
                        "the download of {hak}.hak stopped at {} of {} MB: run it again to go \
                         on from there",
                        mb(have),
                        mb(w.bytes)
                    ));
                }
                return Ok(Ended::Whole(sink.sha.finish()));
            }
            Ok(Err(e)) => {
                return fail(format!(
                    "the download of {hak}.hak stopped at {} of {} MB ({e}): run it again to go \
                     on from there",
                    mb(from + state.got.load(Ordering::Relaxed)),
                    mb(w.bytes)
                ));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if sh.stopped() => {
                return stop("canceled".into());
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if last.1.elapsed() > STREAM_IDLE => {
                return stop(format!(
                    "the download of {hak}.hak had no bytes for {} seconds: run it again to go \
                     on from there",
                    STREAM_IDLE.as_secs()
                ));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return fail("the download failed");
            }
        }
    }
}

/// `ctx.hak:download(hak, { url, signin, sha256, bytes, above_nwsync })`:
/// `{ downloaded, bytes, resumed }`.
pub(crate) fn download(lua: &Lua, sh: &Rc<Shared>, hak: &str, spec: &Table) -> mlua::Result<Table> {
    let net = network(sh, "ctx.hak:download")?;
    let hak = crate::files::hak_name(hak)?;
    let w = wanted(spec)?;
    let Some(dir) = sh.host.hak_dir() else {
        return fail("ctx.hak:download: there is no hak folder here");
    };
    if w.signin.is_some() {
        sh.net.signin(w.signin.as_deref().unwrap_or_default())?;
    }
    if (sh.haks.borrow().iter()).any(|h| h.name == hak && !h.files.is_empty()) {
        return fail(format!(
            "ctx.hak:download: {hak}: a job downloads a hak before it puts resources into it"
        ));
    }
    reach(sh, &net, &w.url, &sh.net.hosts)?;
    let host = crate::net::Url::parse(&w.url).map(|u| u.host).unwrap_or_default();
    let text = format!(
        "{} wants to download the hak {hak}.hak ({} MB) from {host} into your hak folder (in \
         the place of the one there, which is kept as {hak}.hak.bak). Allow it?",
        sh.who,
        mb(w.bytes)
    );
    crate::files::hak_leave(sh, &hak, text)?;
    let out = lua.create_table()?;
    out.set("bytes", w.bytes)?;
    let raise = |sh: &Shared| {
        let mut above = sh.above_nwsync.borrow_mut();
        if w.above_nwsync && !above.contains(&hak) {
            above.push(hak.clone());
        }
    };
    let path = dir.join(format!("{hak}.hak"));
    let part = dir.join(format!("{hak}.hak.part"));
    // (Downloaded already, by this job or before: nothing to fetch.)
    let this_job = (sh.haks.borrow().iter()).find(|h| h.name == hak).and_then(|h| h.whole.clone());
    if this_job.as_deref().is_some_and(|p| is(p, &w)) || (this_job.is_none() && is(&path, &w)) {
        raise(sh);
        out.set("downloaded", false)?;
        out.set("resumed", 0)?;
        return Ok(out);
    }
    let Some(_claim) = Claim::take(&part) else {
        return fail(format!("{hak}.hak is being downloaded already"));
    };
    let have = std::fs::metadata(&part).map_or(0, |m| m.len());
    let (mut from, mut sha) = match hash_of(&part, have) {
        Some(sha) if have > 0 && have <= w.bytes => (have, sha),
        _ => (0, Sha256::new()),
    };
    let resumed = from;
    let mut refreshed = false;
    let mut again = false;
    loop {
        let sum = if from == w.bytes {
            sha.clone().finish()
        } else {
            let mut headers = Vec::new();
            if from > 0 {
                headers.push(("Range".to_string(), format!("bytes={from}-")));
            }
            if let Some(id) = &w.signin {
                let decl = sh.net.signin(id)?.clone();
                let token = crate::signin::bearer(sh, &net, &decl, refreshed)?;
                headers.push(("Authorization".into(), format!("Bearer {token}")));
            }
            let request =
                Request { method: "GET".into(), url: w.url.clone(), headers, body: Vec::new() };
            match fetch(sh, &net, &hak, &part, request, (from, sha.clone()), &w)? {
                Ended::Whole(sum) => sum,
                // (Its token refused: refreshed once, and asked again.)
                Ended::Refused(401, _) if w.signin.is_some() && !refreshed => {
                    refreshed = true;
                    continue;
                }
                // (The part is past what the server has: from the start.)
                Ended::Refused(416, _) if from > 0 => {
                    (from, sha) = (0, Sha256::new());
                    continue;
                }
                Ended::Refused(status, said) => {
                    let said = if said.is_empty() { String::new() } else { format!(": {said}") };
                    return fail(format!("{hak}.hak: the server answered {status}{said}"));
                }
            }
        };
        if sum == w.sha256 {
            break;
        }
        let _ = std::fs::remove_file(&part);
        // (What was there before may have been another hak's start.)
        if from > 0 && !again {
            again = true;
            (from, sha) = (0, Sha256::new());
            continue;
        }
        return fail(format!(
            "{hak}.hak downloaded has the SHA-256 {}, not {}: it is not kept",
            hex(&sum),
            hex(&w.sha256)
        ));
    }
    crate::files::hak_entry(sh, &hak);
    if let Some(h) = sh.haks.borrow_mut().iter_mut().find(|h| h.name == hak) {
        h.whole = Some(part);
    }
    raise(sh);
    out.set("downloaded", true)?;
    out.set("resumed", resumed)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sha_is_64_hex_digits() {
        let s = "ab".repeat(32);
        assert_eq!(parse_sha(&s), Some([0xab; 32]));
        assert_eq!(hex(&[0xab; 32]), s);
        assert!(parse_sha("ab").is_none());
        assert!(parse_sha(&"zz".repeat(32)).is_none());
        assert!(parse_sha(&"é".repeat(32)).is_none());
    }
}
