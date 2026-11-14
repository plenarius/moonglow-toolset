//! The releases this build checks for updates: upstream's
//! (`jadzziaa/moonglow-toolset`), or a fork's, where
//! `packaging/release-source` names one (`repo`, and the `channel` word
//! of its versions). Given to the crate as `MOONGLOW_RELEASE_REPO` and
//! `MOONGLOW_RELEASE_CHANNEL` ([`release::REPO`](src/release.rs)).

const UPSTREAM: &str = "jadzziaa/moonglow-toolset";
const SOURCE: &str = "../../packaging/release-source";

fn main() {
    // (The folder, so that the file is seen as it appears; the file, so
    // that a change to it is.)
    println!("cargo:rerun-if-changed=../../packaging");
    let text = match std::fs::read_to_string(SOURCE) {
        Ok(t) => {
            println!("cargo:rerun-if-changed={SOURCE}");
            t
        }
        Err(_) => String::new(),
    };
    let field = |name: &str| {
        text.lines()
            .map(str::trim)
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| l.split_once('='))
            .find(|(k, _)| k.trim() == name)
            .map(|(_, v)| v.trim().trim_matches('"').to_string())
    };
    let repo = field("repo").unwrap_or_else(|| UPSTREAM.to_string());
    let ok = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
    let parts: Vec<&str> = repo.split('/').collect();
    assert!(
        parts.len() == 2 && parts.iter().all(|p| !p.is_empty() && p.chars().all(ok)),
        "packaging/release-source: repo is to be owner/name, not {repo:?}"
    );
    let channel = field("channel").unwrap_or_default();
    assert!(
        channel.is_empty()
            || (channel.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                && !channel.chars().all(|c| c.is_ascii_digit())),
        "packaging/release-source: channel is to be a word of lowercase letters, not {channel:?}"
    );
    assert!(
        repo == UPSTREAM || !channel.is_empty(),
        "packaging/release-source: a fork's releases need a channel, so that their versions \
         can't be taken for upstream's"
    );
    println!("cargo:rustc-env=MOONGLOW_RELEASE_REPO={repo}");
    println!("cargo:rustc-env=MOONGLOW_RELEASE_CHANNEL={channel}");
}
