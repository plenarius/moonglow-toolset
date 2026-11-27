//! Layonara API 1: `ctx.hak:download`, a whole hak streamed into the hak folder
//! from a declared host, checked by its size and SHA-256, resumed where
//! it stopped, and put in place by the host with the job's haks.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use mg_module::Module;
use mg_plugin::net::{Request, Response, Sink, Transport};
use mg_plugin::{
    Answer, Host, Input, Level, Network, Plugin, Question, SignIns, run_check, run_command,
};

const URL: &str = "https://forge.example.org/hak/tiles";

/// A server of one hak: it answers ranges, and may stop after some bytes
/// or send other bytes than the hak's.
struct Stub {
    hak: Vec<u8>,
    /// The next answer stops after this many bytes (the connection gone).
    cut: Mutex<Option<usize>>,
    /// The next answer is these bytes, as a whole file.
    other: Mutex<Option<Vec<u8>>>,
    /// Ranges are not answered: always the whole file.
    whole_only: bool,
    seen: Mutex<Vec<Request>>,
}

impl Stub {
    fn new(hak: Vec<u8>) -> Stub {
        Stub {
            hak,
            cut: Mutex::default(),
            other: Mutex::default(),
            whole_only: false,
            seen: Mutex::default(),
        }
    }
}

impl Transport for Stub {
    fn send(&self, r: &Request) -> Result<Response, String> {
        self.seen.lock().unwrap().push(r.clone());
        Ok(Response { status: 404, ..Response::default() })
    }

    fn stream(&self, r: &Request, sink: &mut dyn Sink) -> Result<(), String> {
        self.seen.lock().unwrap().push(r.clone());
        if !r.url.starts_with(URL) {
            sink.head(404, &[])?;
            return sink.bytes(b"no such hak");
        }
        let body = self.other.lock().unwrap().take().unwrap_or_else(|| self.hak.clone());
        let range = (r.headers.iter())
            .find(|(k, _)| k.eq_ignore_ascii_case("range"))
            .and_then(|(_, v)| v.strip_prefix("bytes=")?.strip_suffix('-')?.parse::<usize>().ok())
            .filter(|_| !self.whole_only);
        let (status, from) = match range {
            Some(f) if f >= body.len() => {
                sink.head(416, &[])?;
                return Ok(());
            }
            Some(f) => (206, f),
            None => (200, 0),
        };
        let mut headers = vec![("content-length".to_string(), (body.len() - from).to_string())];
        if status == 206 {
            headers.push((
                "content-range".into(),
                format!("bytes {from}-{}/{}", body.len() - 1, body.len()),
            ));
        }
        sink.head(status, &headers)?;
        let cut = self.cut.lock().unwrap().take();
        for chunk in body[from..].chunks(1000).enumerate() {
            if cut.is_some_and(|c| chunk.0 * 1000 >= c) {
                return Err("the connection was reset".into());
            }
            sink.bytes(chunk.1)?;
        }
        Ok(())
    }
}

struct DlHost {
    network: Network,
    dir: PathBuf,
    yes: bool,
    asked: RefCell<Vec<String>>,
    progress: RefCell<usize>,
}

impl Host for DlHost {
    fn log(&self, _: Level, _: &str) {}

    fn progress(&self, _: usize, _: usize, _: &str) {
        *self.progress.borrow_mut() += 1;
    }

    fn ask(&self, q: &Question) -> Option<Answer> {
        if let Question::Confirm(text) = q {
            self.asked.borrow_mut().push(text.clone());
        }
        Some(if self.yes { Answer::Yes } else { Answer::No })
    }

    fn network(&self) -> Option<Network> {
        Some(self.network.clone())
    }

    fn hak_dir(&self) -> Option<PathBuf> {
        Some(self.dir.clone())
    }
}

fn sha(bytes: &[u8]) -> String {
    mg_core::sha256::sha256(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// A plugin whose command downloads the hak `tiles` with the size and
/// SHA-256 given, and logs what it was told.
fn plugin(dir: &Path, sha256: &str, bytes: usize) -> Plugin {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("plugin.cfg"),
        "[plugin]\nid = \"t.dl\"\nname = \"Dl\"\nversion = \"1\"\napi = \"0.10\"\n\
         layonara_api = \"1\"\nlicense = \"MIT\"\n\n[network]\nhosts = \"forge.example.org\"\n\n\
         [signin]\nid = \"forge\"\nissuer = \"https://forge.example.org/o/\"\n\
         client = \"dl\"\nscopes = \"openid\"\n\n\
         [command]\nid = \"get\"\ntitle = \"Get\"\n\n[check]\nid = \"c\"\ntitle = \"C\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.luau"),
        format!(
            "local mg = require('@moonglow')\n\
             local spec = {{ url = '{URL}', signin = 'forge', sha256 = '{sha256}', bytes = {bytes}, above_nwsync = true }}\n\
             mg.command('get', function(ctx)\n\
               local got = ctx.hak:download('tiles', spec)\n\
               ctx.hak:attach('tiles')\n\
               ctx.log:info(tostring(got.downloaded) .. ' ' .. got.bytes .. ' ' .. got.resumed)\n\
             end)\n\
             mg.check('c', function(ctx)\n\
               ctx.hak:download('tiles', spec)\n\
               return {{}}\n\
             end)\n"
        ),
    )
    .unwrap();
    Plugin::load(dir).unwrap()
}

fn module() -> Module {
    let mut m = Module::new();
    m.set_info(&mg_gff::Gff::new(*b"IFO ")).unwrap();
    m
}

/// A hak's bytes, not all alike.
fn hak_bytes(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 + i / 251) as u8).collect()
}

fn host(stub: &Arc<Stub>, dir: &Path) -> DlHost {
    let sign_ins = Arc::new(SignIns::in_memory());
    let probe = dir.parent().unwrap().join("decl");
    let decl = plugin(&probe, &"00".repeat(32), 1).manifest.signins[0].clone();
    sign_ins.hold(&mg_plugin::signin::account("t.dl", &decl), "alice", "TOKEN");
    DlHost {
        network: Network::new(stub.clone(), sign_ins),
        dir: dir.to_path_buf(),
        yes: true,
        asked: RefCell::default(),
        progress: RefCell::default(),
    }
}

/// Downloaded with the sign-in's token, checked, and put in place with
/// the old hak kept; a second run finds it there and downloads nothing.
#[test]
fn a_hak_is_downloaded_checked_and_put_in_place() {
    let root = mg_testkit::scratch_dir("plugin-download-whole");
    let _ = std::fs::remove_dir_all(&root);
    let haks = root.join("hak");
    std::fs::create_dir_all(&haks).unwrap();
    std::fs::write(haks.join("tiles.hak"), b"the old one").unwrap();
    let bytes = hak_bytes(25_000);
    let stub = Arc::new(Stub::new(bytes.clone()));
    let p = plugin(&root.join("plugin"), &sha(&bytes), bytes.len());
    let h = Rc::new(host(&stub, &haks));
    let out = run_command(&p, "get", Input::new(module(), None), h.clone()).unwrap();
    assert_eq!(h.asked.borrow().len(), 1, "asked once: {:?}", h.asked.borrow());
    assert!(h.asked.borrow()[0].contains("download the hak tiles.hak"));
    assert!(*h.progress.borrow() > 0, "the progress is shown");
    let sent = stub.seen.lock().unwrap()[0].clone();
    assert!(sent.headers.contains(&("Authorization".into(), "Bearer TOKEN".into())));
    assert_eq!(out.above_nwsync, ["tiles"]);
    assert_eq!(out.haks.len(), 1);
    let part = out.haks[0].whole.clone().expect("downloaded whole");
    assert_eq!(part, haks.join("tiles.hak.part"));
    assert_eq!(std::fs::read(haks.join("tiles.hak")).unwrap(), b"the old one", "not yet");
    // The host puts it in place, the old one kept; Undo puts that back.
    let mut done = out.haks[0].write_kept(&haks).unwrap();
    assert!(done.whole);
    assert_eq!(std::fs::read(haks.join("tiles.hak")).unwrap(), bytes);
    assert_eq!(std::fs::read(haks.join("tiles.hak.bak")).unwrap(), b"the old one");
    assert!(!part.exists());
    done.undo().unwrap();
    assert_eq!(std::fs::read(haks.join("tiles.hak")).unwrap(), b"the old one");
    done.redo().unwrap();
    assert_eq!(std::fs::read(haks.join("tiles.hak")).unwrap(), bytes);
    // There already: nothing fetched, nothing written, still above NWSync.
    let before = stub.seen.lock().unwrap().len();
    let h = Rc::new(host(&stub, &haks));
    let out = run_command(&p, "get", Input::new(module(), None), h).unwrap();
    assert_eq!(stub.seen.lock().unwrap().len(), before);
    assert!(out.haks.is_empty());
    assert_eq!(out.above_nwsync, ["tiles"]);
}

/// Cut short, the part stays; the next run asks for the rest and ends
/// with the whole hak. A server that ignores the range starts it over.
#[test]
fn a_download_cut_short_goes_on_from_where_it_stopped() {
    let root = mg_testkit::scratch_dir("plugin-download-resume");
    let _ = std::fs::remove_dir_all(&root);
    let haks = root.join("hak");
    std::fs::create_dir_all(&haks).unwrap();
    let bytes = hak_bytes(40_000);
    let stub = Arc::new(Stub::new(bytes.clone()));
    let p = plugin(&root.join("plugin"), &sha(&bytes), bytes.len());
    *stub.cut.lock().unwrap() = Some(15_000);
    let e = run_command(&p, "get", Input::new(module(), None), Rc::new(host(&stub, &haks)))
        .unwrap_err()
        .to_string();
    assert!(e.contains("stopped at") && e.contains("run it again"), "{e}");
    assert_eq!(std::fs::metadata(haks.join("tiles.hak.part")).unwrap().len(), 15_000);
    let out =
        run_command(&p, "get", Input::new(module(), None), Rc::new(host(&stub, &haks))).unwrap();
    let last = stub.seen.lock().unwrap().last().cloned().unwrap();
    assert!(last.headers.contains(&("Range".into(), "bytes=15000-".into())), "{last:?}");
    assert_eq!(std::fs::read(out.haks[0].whole.as_ref().unwrap()).unwrap(), bytes);

    // (Whole answers only: the part is written over.)
    let _ = std::fs::remove_file(haks.join("tiles.hak.part"));
    let stub = Arc::new(Stub { whole_only: true, ..Stub::new(bytes.clone()) });
    *stub.cut.lock().unwrap() = Some(9_000);
    let _ = run_command(&p, "get", Input::new(module(), None), Rc::new(host(&stub, &haks)));
    let out =
        run_command(&p, "get", Input::new(module(), None), Rc::new(host(&stub, &haks))).unwrap();
    assert_eq!(std::fs::read(out.haks[0].whole.as_ref().unwrap()).unwrap(), bytes);
}

/// Other bytes than the SHA-256 says are not kept; a part that was
/// another hak's start is tried once from the beginning.
#[test]
fn a_hak_of_another_sha_256_is_not_kept() {
    let root = mg_testkit::scratch_dir("plugin-download-sha");
    let _ = std::fs::remove_dir_all(&root);
    let haks = root.join("hak");
    std::fs::create_dir_all(&haks).unwrap();
    let bytes = hak_bytes(12_000);
    let stub = Arc::new(Stub::new(bytes.clone()));
    let p = plugin(&root.join("plugin"), &sha(&bytes), bytes.len());
    let mut wrong = bytes.clone();
    wrong[5000] ^= 1;
    *stub.other.lock().unwrap() = Some(wrong);
    let e = run_command(&p, "get", Input::new(module(), None), Rc::new(host(&stub, &haks)))
        .unwrap_err()
        .to_string();
    assert!(e.contains("SHA-256") && e.contains("not kept"), "{e}");
    assert!(!haks.join("tiles.hak.part").exists());
    // Another hak's start in the part: resumed, wrong, then whole again.
    std::fs::write(haks.join("tiles.hak.part"), b"something else entirely").unwrap();
    let out =
        run_command(&p, "get", Input::new(module(), None), Rc::new(host(&stub, &haks))).unwrap();
    assert_eq!(std::fs::read(out.haks[0].whole.as_ref().unwrap()).unwrap(), bytes);
}

/// What is refused: no leave, a check, a size past the limit, an
/// undeclared host, an error status.
#[test]
fn a_download_is_a_command_s_with_leave_and_limits() {
    let root = mg_testkit::scratch_dir("plugin-download-refused");
    let _ = std::fs::remove_dir_all(&root);
    let haks = root.join("hak");
    std::fs::create_dir_all(&haks).unwrap();
    let bytes = hak_bytes(3000);
    let stub = Arc::new(Stub::new(bytes.clone()));
    let p = plugin(&root.join("plugin"), &sha(&bytes), bytes.len());
    let mut no = host(&stub, &haks);
    no.yes = false;
    let e = run_command(&p, "get", Input::new(module(), None), Rc::new(no)).unwrap_err();
    assert!(
        e.to_string().contains("not allowed") || e.to_string().contains("did not allow"),
        "{e}"
    );
    let e = run_check(&p, "c", Input::new(module(), None), Rc::new(host(&stub, &haks)));
    assert!(e.unwrap_err().to_string().contains("is for commands and wizards"));
    let big = plugin(&root.join("big"), &sha(&bytes), 600 << 20);
    let e = run_command(&big, "get", Input::new(module(), None), Rc::new(host(&stub, &haks)));
    assert!(e.unwrap_err().to_string().contains("512 MB"));
    let bad = plugin(&root.join("bad"), "abc", 10);
    let e = run_command(&bad, "get", Input::new(module(), None), Rc::new(host(&stub, &haks)));
    assert!(e.unwrap_err().to_string().contains("is not a SHA-256"));
    // (Another URL of the host: the stub answers 404.)
    let main = std::fs::read_to_string(root.join("plugin/main.luau")).unwrap();
    std::fs::write(root.join("plugin/main.luau"), main.replace("/hak/tiles", "/hak/gone")).unwrap();
    let p = Plugin::load(&root.join("plugin")).unwrap();
    let e = run_command(&p, "get", Input::new(module(), None), Rc::new(host(&stub, &haks)));
    let e = e.unwrap_err().to_string();
    assert!(e.contains("answered 404: no such hak"), "{e}");
    std::fs::write(
        root.join("plugin/main.luau"),
        main.replace("https://forge.example.org", "https://evil.example.org"),
    )
    .unwrap();
    let p = Plugin::load(&root.join("plugin")).unwrap();
    let e = run_command(&p, "get", Input::new(module(), None), Rc::new(host(&stub, &haks)));
    assert!(e.unwrap_err().to_string().contains("not a host the manifest declares"));
}

/// `mg plugin test`: a download gets the reply's file into a scratch hak
/// folder, and the test holds the job to what it downloaded.
#[test]
fn a_plugin_s_test_downloads_from_its_stub() {
    let root = mg_testkit::scratch_dir("plugin-download-test-file");
    let bytes = hak_bytes(5000);
    let dir = root.join("plugin");
    let _ = plugin(&dir, &sha(&bytes), bytes.len());
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(dir.join("tests/tiles.bin"), &bytes).unwrap();
    let fixture = mg_module::ModuleLocation::Archive(dir.join("tests/fixture.mod"));
    module().save_as(&fixture).unwrap();
    std::fs::write(
        dir.join("tests/dl.toml"),
        format!(
            "module = \"fixture.mod\"\n\n[[test]]\nname = \"fetches\"\ncommand = \"get\"\nyes = true\nsigned_in = \"alice\"\n\
             downloaded = [\"tiles\"]\nabove_nwsync = [\"tiles\"]\nlog = [\"true 5000 0\"]\n\
             [[test.reply]]\nurl = \"{URL}\"\nfile = \"tiles.bin\"\n\
             [[test.request]]\nurl = \"{URL}\"\nsignin = true\n\n\
             [[test]]\nname = \"wrong\"\ncommand = \"get\"\nyes = true\nsigned_in = \"alice\"\n\
             downloaded = []\n\
             [[test.reply]]\nurl = \"{URL}\"\nfile = \"tiles.bin\"\n"
        ),
    )
    .unwrap();
    let p = Plugin::load(&dir).unwrap();
    let results = mg_plugin::run_tests(&p, None, &|_, _| Ok(None));
    let of = |name: &str| results.iter().find(|r| r.name == name).unwrap().problems.clone();
    assert_eq!(of("fetches"), Vec::<String>::new());
    assert_eq!(of("wrong"), ["it downloads [\"tiles\"], not []"]);
}
