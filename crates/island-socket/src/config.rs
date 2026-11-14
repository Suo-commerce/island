//! `island-socket`'s config file (docs/slice-2.md, "Providers"), TOML,
//! read once at start: where it listens, whom it writes as, and the model
//! providers with their budgets. Switching a provider is an edit here and
//! a restart; the agent and its Vessel are untouched.
//!
//! ```toml
//! socket = "/var/lib/taso/agent-island/run/island/island.sock"
//! warden_config = "/etc/taso/warden.json"
//! state_dir = "/var/lib/island-socket"
//! broker = "http://10.77.0.1:4747"
//! root = "a3d2…"                      # the pinned root public key, hex
//! key = "/etc/island-socket/socket.key"
//! access_every_secs = 30
//! default = "local"
//!
//! [provider.local]
//! base_url = "http://127.0.0.1:8080/v1"
//! models = ["qwen3.6-35b-a3b"]
//! key_file = "/etc/island-socket/local.key"   # optional
//! budget = { calls = 2000, bytes = 500_000_000 }
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The whole file.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The socket it serves, in the Vessels' root filesystem.
    pub socket: PathBuf,
    /// Taso's Warden config: who the callers are.
    pub warden_config: PathBuf,
    /// Where it keeps the day's usage, so a restart is no way past a
    /// budget.
    pub state_dir: PathBuf,
    /// The broker its `access` entries go to.
    pub broker: String,
    /// The pinned root public key, in hex.
    pub root: String,
    /// Its writer key file: one grant, `access` on `access`.
    pub key: PathBuf,
    /// How often the batched `access` entries are written, in seconds.
    #[serde(default = "thirty")]
    pub access_every_secs: u64,
    /// The provider used when the agent names no model.
    pub default: String,
    /// The providers, by name.
    #[serde(default)]
    pub provider: BTreeMap<String, Provider>,
}

fn thirty() -> u64 {
    30
}

/// One OpenAI-compatible endpoint.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    /// The API's base, ending before `/chat/completions`, e.g.
    /// `http://127.0.0.1:8080/v1`.
    pub base_url: String,
    /// The models it may be asked for: the agent's whole choice.
    pub models: Vec<String>,
    /// A file holding the API key, sent as a bearer token; readable by
    /// `island-socket` only.
    #[serde(default)]
    pub key_file: Option<PathBuf>,
    /// Each Vessel's allowance per UTC day, with this provider.
    pub budget: Budget,
}

/// A day's allowance.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    /// Calls.
    pub calls: u64,
    /// Bytes, both ways together.
    pub bytes: u64,
}

impl Config {
    /// Read and check the file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        Self::parse(&text).map_err(|error| format!("{}: {error}", path.display()))
    }

    /// Parse and check.
    pub fn parse(text: &str) -> Result<Self, String> {
        let config: Self = toml::from_str(text).map_err(|error| error.to_string())?;
        config.check()?;
        Ok(config)
    }

    fn check(&self) -> Result<(), String> {
        if island_env::keyfile::unhex::<32>(&self.root).is_none() {
            return Err("`root` is not a public key (64 hex digits)".to_owned());
        }
        if !self.provider.contains_key(&self.default) {
            return Err(format!("`default` names no provider: {:?}", self.default));
        }
        if self.access_every_secs == 0 {
            return Err("`access_every_secs` must be at least 1".to_owned());
        }
        let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
        for (name, provider) in &self.provider {
            if name.is_empty() || name.len() > 64 {
                return Err(format!("provider name {name:?}: 1 to 64 bytes"));
            }
            if provider.models.is_empty() {
                return Err(format!("provider {name}: no models"));
            }
            if let Some(rest) = provider.base_url.strip_prefix("https://") {
                // reqwest is built without TLS in this workspace; a hosted
                // provider comes with it (docs/slice-2.md, step 3).
                return Err(format!(
                    "provider {name}: https (https://{rest}) needs TLS, which island-socket \
                     is not built with yet"
                ));
            }
            if !provider.base_url.starts_with("http://") {
                return Err(format!("provider {name}: base_url is not http://"));
            }
            for model in &provider.models {
                if let Some(other) = seen.insert(model, name) {
                    return Err(format!(
                        "model {model:?} is listed by both {other} and {name}: the agent \
                         names a model, so each is one provider's"
                    ));
                }
            }
        }
        Ok(())
    }

    /// The provider that lists `model`, or the default with its first
    /// model when the agent names none. `None`: no provider lists it.
    pub fn route<'a>(&'a self, model: Option<&'a str>) -> Option<(&'a str, &'a Provider, &'a str)> {
        match model {
            None => {
                let provider = &self.provider[&self.default];
                Some((&self.default, provider, &provider.models[0]))
            }
            Some(model) => self.provider.iter().find_map(|(name, provider)| {
                provider.models.iter().any(|m| m == model).then_some((
                    name.as_str(),
                    provider,
                    model,
                ))
            }),
        }
    }

    /// Every model the agent may name.
    pub fn models(&self) -> Vec<(&str, &str)> {
        self.provider
            .iter()
            .flat_map(|(name, provider)| {
                provider
                    .models
                    .iter()
                    .map(move |model| (name.as_str(), model.as_str()))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "a3d2b14059647d18c5334b853e1f712e62d1ddae66f959a3224bf5130acf7e5d";

    fn text(providers: &str) -> String {
        format!(
            "socket = \"/s\"\nwarden_config = \"/w\"\nstate_dir = \"/d\"\nbroker = \"http://b\"\n\
             root = \"{ROOT}\"\nkey = \"/k\"\ndefault = \"local\"\n{providers}"
        )
    }

    const TWO: &str = "[provider.local]\nbase_url = \"http://127.0.0.1:8080/v1\"\nmodels = [\"a\", \"b\"]\n\
                       budget = { calls = 10, bytes = 1000 }\n\
                       [provider.gpu]\nbase_url = \"http://10.77.0.4:8000/v1\"\nmodels = [\"c\"]\n\
                       budget = { calls = 5, bytes = 500 }\n";

    #[test]
    fn a_model_finds_its_provider_and_none_the_default() {
        let config = Config::parse(&text(TWO)).unwrap();
        assert_eq!(config.access_every_secs, 30);
        assert_eq!(
            config.route(Some("c")).map(|(p, _, m)| (p, m)),
            Some(("gpu", "c"))
        );
        assert_eq!(
            config.route(Some("b")).map(|(p, _, m)| (p, m)),
            Some(("local", "b"))
        );
        assert_eq!(
            config.route(None).map(|(p, _, m)| (p, m)),
            Some(("local", "a"))
        );
        assert!(config.route(Some("gpt-anything")).is_none());
        assert_eq!(config.models().len(), 3);
    }

    #[test]
    fn a_bad_file_is_refused_with_its_reason() {
        let bad = |providers: &str, want: &str| {
            let error = Config::parse(&text(providers)).unwrap_err();
            assert!(error.contains(want), "{error}");
        };
        bad("", "`default` names no provider");
        bad(
            "[provider.local]\nbase_url = \"https://api.example.com/v1\"\nmodels = [\"a\"]\nbudget = { calls = 1, bytes = 1 }\n",
            "needs TLS",
        );
        bad(
            "[provider.local]\nbase_url = \"http://x/v1\"\nmodels = []\nbudget = { calls = 1, bytes = 1 }\n",
            "no models",
        );
        bad(
            "[provider.local]\nbase_url = \"http://x/v1\"\nmodels = [\"a\"]\nbudget = { calls = 1, bytes = 1 }\n\
             [provider.other]\nbase_url = \"http://y/v1\"\nmodels = [\"a\"]\nbudget = { calls = 1, bytes = 1 }\n",
            "listed by both",
        );
        bad(
            "[provider.local]\nbase_url = \"http://x/v1\"\nmodels = [\"a\"]\nbudget = { calls = 1, bytes = 1 }\nextra = 1\n",
            "unknown field",
        );
    }
}
