//! On Windows, the executable's icon and version information; and the key
//! the releases' checksums are signed with, where the owner has committed
//! it (`packaging/minisign.pub`, or a fork's named in
//! `packaging/release-source`; `packaging/README.md`); and the plugins
//! the build bundles (`plugins/`, each folder one, without its `tests/`).

fn main() {
    println!("cargo:rerun-if-changed=../../packaging/icons/moonglow.ico");
    update_key();
    bundle_plugins();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../packaging/icons/moonglow.ico")
        .set("ProductName", "Moonglow Toolset")
        .set("FileDescription", "Moonglow Toolset")
        .set("LegalCopyright", "GPL-3.0-only");
    // Without a resource compiler (cross builds), the program still builds,
    // only without its icon.
    if let Err(e) = res.compile() {
        println!("cargo:warning=no Windows resources (icon, version): {e}");
    }
}

/// The key the releases' checksums are signed with, given to the program
/// as `MOONGLOW_UPDATE_KEY` (its key's line): upstream's build reads
/// `packaging/minisign.pub` (as `minisign -G` writes it: an untrusted
/// comment, then the key); a fork's (`packaging/release-source` names
/// its repository) reads the file its `key` names in `packaging/`, and
/// never upstream's, whose releases it doesn't install. Without the file
/// the build has no key, and installs no update (the window links the
/// release's page).
fn update_key() {
    // (The folder, so that the file is seen as it appears.)
    println!("cargo:rerun-if-changed=../../packaging");
    let source = std::fs::read_to_string("../../packaging/release-source").ok();
    if source.is_some() {
        println!("cargo:rerun-if-changed=../../packaging/release-source");
    }
    let file = match &source {
        None => "minisign.pub".to_string(),
        Some(text) => {
            let key = text
                .lines()
                .map(str::trim)
                .filter(|l| !l.starts_with('#'))
                .filter_map(|l| l.split_once('='))
                .find(|(k, _)| k.trim() == "key")
                .map(|(_, v)| v.trim().trim_matches('"').to_string());
            match key {
                Some(k) if !k.is_empty() && !k.contains(['/', '\\']) && k != "minisign.pub" => k,
                Some(k) if !k.is_empty() => panic!(
                    "packaging/release-source: key is a file in packaging/ of the fork's own, not \
                     {k:?}"
                ),
                _ => return,
            }
        }
    };
    let path = format!("../../packaging/{file}");
    println!("cargo:rerun-if-changed={path}");
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let key = text
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty() && !l.starts_with("untrusted comment:"))
        .unwrap_or_default();
    let base64 = |c: char| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=');
    // (A minisign public key is 56 characters of base64.)
    assert!(
        key.len() == 56 && key.chars().all(base64),
        "packaging/{file} holds no minisign public key"
    );
    println!("cargo:rustc-env=MOONGLOW_UPDATE_KEY={key}");
}

/// The plugins of `plugins/` at the source's top, as `bundled.rs` in
/// OUT_DIR: each plugin's files (path in its folder, bytes), which the
/// program installs at its first start (`mg_ui::plugins`).
fn bundle_plugins() {
    use std::fmt::Write as _;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    println!("cargo:rerun-if-changed=../../plugins");
    let mut out = String::from("&[\n");
    let mut dirs: Vec<_> = std::fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("plugin.cfg").is_file())
        .collect();
    dirs.sort();
    for dir in dirs {
        let mut files = Vec::new();
        walk(&dir, &dir, &mut files);
        files.sort();
        out.push_str("    &[\n");
        for (name, path) in files {
            let path = path.canonicalize().unwrap_or(path);
            let _ = writeln!(
                out,
                "        ({name:?}, include_bytes!({:?})),",
                path.display().to_string()
            );
        }
        out.push_str("    ],\n");
    }
    out.push(']');
    let dest = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("bundled.rs");
    std::fs::write(dest, out).unwrap();
}

/// A plugin's files under `dir`: not hidden ones, nor its `tests/`.
fn walk(top: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, std::path::PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || (dir == top && name == "tests") {
            continue;
        }
        if path.is_dir() {
            walk(top, &path, out);
        } else if path.is_file() {
            let rel = path.strip_prefix(top).unwrap().to_string_lossy().replace('\\', "/");
            out.push((rel, path));
        }
    }
}
