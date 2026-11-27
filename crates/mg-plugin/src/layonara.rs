//! The Layonara build's own plugin API (a manifest's `layonara_api`),
//! numbered apart from upstream's `api` so that the two never collide.
//!
//! Level 1: `[network]`, `[signin]`, `mg.json`, the JSON of GFFs,
//! `ctx.hak:download`, `[command] new = "module"`, `folder:unzip` and
//! `folder:open`.

use crate::manifest::Manifest;

/// The Layonara API this Moonglow serves (a manifest's `layonara_api`).
pub const API: &str = "1";
/// The Layonara APIs a plugin may be written for: each later one has all
/// of the ones before.
pub const APIS: [&str; 1] = ["1"];

/// A manifest's `layonara_api`: empty when it has none.
pub(crate) fn parse(given: &str) -> Result<String, String> {
    if given.is_empty() || APIS.contains(&given) {
        Ok(given.to_string())
    } else {
        Err(format!(
            "it was written for Layonara API {given}; this Moonglow has Layonara API {API}"
        ))
    }
}

/// Refuses a manifest that uses what the Layonara API added without
/// naming it: `section` is its first `[network]` or `[signin]`.
pub(crate) fn require(m: &Manifest, section: Option<&(usize, String)>) -> Result<(), String> {
    if !m.layonara_api.is_empty() {
        return Ok(());
    }
    if let Some(c) = m.commands.iter().find(|c| c.new_module) {
        return Err(format!(
            "the command {:?} makes a module (`new = \"module\"`), which is Layonara API 1; \
             the manifest has no layonara_api = \"1\"",
            c.id
        ));
    }
    if let Some((line, name)) = section {
        return Err(format!(
            "line {line}: [{name}] is Layonara API 1; the manifest has no layonara_api = \"1\""
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::Manifest;

    const PLUGIN: &str = "[plugin]\nid = \"a.b\"\nname = \"B\"\nversion = \"1\"\n\
                          api = \"0.10\"\nlayonara_api = \"1\"\nlicense = \"MIT\"\n";

    fn with(extra: &str) -> Result<Manifest, String> {
        Manifest::parse(&format!("{PLUGIN}{extra}"))
    }

    fn without(extra: &str) -> Result<Manifest, String> {
        Manifest::parse(&format!("{}{extra}", PLUGIN.replace("layonara_api = \"1\"\n", "")))
    }

    /// An upstream manifest, with none of the fork's features, loads as it is.
    #[test]
    fn an_upstream_manifest_needs_no_layonara_api() {
        let m = without("[command]\nid = \"go\"\ntitle = \"Go\"\n").unwrap();
        assert_eq!((m.api.as_str(), m.layonara_api.as_str()), ("0.10", ""));
        assert_eq!(with("").unwrap().layonara_api, "1");
    }

    /// Upstream's numbering is upstream's: the fork's old 0.11 and 0.12 are gone.
    #[test]
    fn api_is_upstreams_and_layonara_api_the_forks() {
        assert_eq!((crate::API, super::API), ("0.10", "1"));
        for old in ["0.11", "0.12"] {
            let e = Manifest::parse(&PLUGIN.replace("0.10", old)).unwrap_err();
            assert!(e.contains(&format!("plugin API {old}")), "{e}");
        }
        let e = Manifest::parse(&PLUGIN.replace("layonara_api = \"1\"", "layonara_api = \"2\""));
        assert!(e.unwrap_err().contains("Layonara API 2"));
    }

    /// `new = "module"`, `[network]` and `[signin]` need `layonara_api`.
    #[test]
    fn a_fork_feature_needs_layonara_api() {
        let new = "[command]\nid = \"get\"\ntitle = \"Get…\"\nnew = \"module\"\n\
                   [command]\nid = \"other\"\ntitle = \"Other\"\n";
        let m = with(new).unwrap();
        assert!(m.commands[0].new_module && !m.commands[1].new_module);
        let e = without(new).unwrap_err();
        assert!(
            e.contains("Layonara API 1") && e.contains("\"get\"") && e.contains("layonara_api")
        );
        let e = with(&new.replace("new = \"module\"", "new = \"area\"")).unwrap_err();
        assert!(e.contains("new is \"module\""), "{e}");

        let net = "[network]\nhosts = \"a.example.org\"\n";
        assert_eq!(with(net).unwrap().network, ["a.example.org"]);
        let e = without(net).unwrap_err();
        assert!(e.contains("[network] is Layonara API 1") && e.contains("layonara_api"), "{e}");
        let signin = "[signin]\nid = \"a\"\nissuer = \"https://a.example.org/\"\nclient = \"c\"\n";
        assert_eq!(with(signin).unwrap().signins[0].id, "a");
        assert!(without(signin).unwrap_err().contains("[signin] is Layonara API 1"));
    }

    /// The hosts a plugin talks to, and the services it signs in to; held
    /// to plain names and https.
    #[test]
    fn a_plugin_declares_its_hosts_and_sign_ins() {
        let m = with(
            "[network]\nhosts = \"builders.layonara.com, files.example.org\"\n\n[signin]\n\
             id = \"layonara\"\nissuer = \"https://auth.layonara.com/application/o/x/\"\n\
             client = \"x\"\nscopes = \"openid  profile offline_access\"\n",
        )
        .unwrap();
        assert_eq!(m.network, ["builders.layonara.com", "files.example.org"]);
        assert_eq!(m.signins[0].scopes, "openid profile offline_access");
        assert_eq!(m.signins[0].host(), "auth.layonara.com");
        assert_eq!(m.hosts(), ["builders.layonara.com", "files.example.org", "auth.layonara.com"]);
        for bad in ["*.example.org", "10.0.0.1", "example.org:443", "Example.org", "localhost", ""]
        {
            let e = with(&format!("[network]\nhosts = \"{bad}\"\n")).unwrap_err();
            assert!(e.contains("host") || e.contains("needs hosts"), "{bad}: {e}");
        }
        let e =
            with("[signin]\nid = \"a\"\nissuer = \"http://auth.example.org/\"\nclient = \"c\"\n");
        assert!(e.unwrap_err().contains("https://"));
        let e = with("[signin]\nid = \"a\"\nissuer = \"https://a.org/\"\n").unwrap_err();
        assert!(e.contains("needs client"), "{e}");
    }
}
