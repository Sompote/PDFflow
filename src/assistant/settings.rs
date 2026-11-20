//! The Assistant's settings (ported from PDFflow): which provider runs the agent (an
//! OpenAI-compatible API through th-llm, or Claude Code / Codex as the model), its model, key
//! and options. They live in the PDFflow settings folder as `agent_settings.json`; API keys sit
//! beside it in owner-only files (`api_key_<provider>`), and nothing is written until the user
//! presses Save.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use th_llm::Provider;

/// The PDFflow settings folder.
pub fn settings_dir() -> Option<PathBuf> {
    let home = std::env::var_os(if cfg!(windows) { "APPDATA" } else { "HOME" }).map(PathBuf::from)?;
    Some(if cfg!(target_os = "macos") {
        home.join("Library/Application Support/PDFflow")
    } else if cfg!(windows) {
        home.join("PDFflow")
    } else {
        home.join(".config/pdfflow")
    })
}

/// The providers the Settings card offers, in its order: (id, label).
pub const PROVIDERS: [(&str, &str); 7] = [
    ("deepseek", "DeepSeek"),
    ("openai", "OpenAI-compatible"),
    ("openrouter", "OpenRouter"),
    ("ollama", "Ollama"),
    ("vllm", "vLLM"),
    ("claude-code", "Claude Code"),
    ("codex", "Codex"),
];

pub fn label(id: &str) -> &'static str {
    PROVIDERS.iter().find(|(i, _)| *i == id).map(|(_, l)| *l).unwrap_or("Provider")
}

/// One provider's own settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderSettings {
    /// Empty: the preset's base URL.
    pub base_url: String,
    /// Empty: the preset's default model (Claude Code / Codex: the program's own default).
    pub model: String,
    /// Whether the model takes images: "auto", "yes" or "no".
    pub vision: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSettings {
    /// One of `PROVIDERS`.
    pub provider: String,
    pub providers: BTreeMap<String, ProviderSettings>,
    /// Let the agent hand a self-contained part of the work to a helper agent (th-hive
    /// `delegate`); off: one agent does everything.
    pub subagents: bool,
    /// Model requests of one message before the agent is stopped.
    pub max_steps: usize,
    /// Claude Code / Codex: the program (empty: found on PATH or in the usual places).
    pub claude_path: String,
    pub codex_path: String,
    /// Run Claude Code / Codex on the subscription they are logged in with: ANTHROPIC_API_KEY
    /// (OPENAI_API_KEY for Codex) is removed from their environment.
    pub subscription: bool,
    /// Minutes one Claude Code / Codex run may take.
    pub cli_timeout_min: u64,
}

impl Default for AgentSettings {
    fn default() -> Self {
        AgentSettings { provider: "deepseek".into(), providers: BTreeMap::new(), subagents: false, max_steps: 60, claude_path: String::new(), codex_path: String::new(), subscription: true, cli_timeout_min: 30 }
    }
}

impl AgentSettings {
    pub fn current(&self) -> ProviderSettings {
        self.providers.get(&self.provider).cloned().unwrap_or_default()
    }

    pub fn current_mut(&mut self) -> &mut ProviderSettings {
        let id = self.provider.clone();
        self.providers.entry(id).or_default()
    }

    pub fn is_cli(&self) -> bool {
        matches!(self.provider.as_str(), "claude-code" | "codex")
    }

    /// The th-llm provider for the chosen one, with its key (from `keys`).
    pub fn llm_provider(&self, keys: &Keys) -> Provider {
        let mut p = preset(&self.provider);
        let ps = self.current();
        if !ps.base_url.trim().is_empty() {
            p.base_url = ps.base_url.trim().trim_end_matches('/').to_string();
        }
        if let Some(k) = keys.get(&self.provider) {
            p.api_key = k;
        }
        match ps.vision.as_str() {
            "yes" => p.vision = true,
            "no" => p.vision = false,
            _ => {}
        }
        p
    }

    /// The model asked for: the setting, else the preset's default (may be empty).
    pub fn model(&self) -> String {
        let m = self.current().model.trim().to_string();
        if m.is_empty() {
            preset(&self.provider).default_model
        } else {
            m
        }
    }

    /// What the header chip shows: "OpenRouter · deepseek/deepseek-v4.1-flash".
    pub fn chip(&self) -> String {
        let m = self.model();
        let m = m.rsplit('/').next().unwrap_or(&m).to_string();
        if m.is_empty() {
            label(&self.provider).to_string()
        } else {
            format!("{} · {m}", label(&self.provider))
        }
    }
}

/// The th-llm preset of a provider id ("openai" is any OpenAI-compatible API).
pub fn preset(id: &str) -> Provider {
    match id {
        "deepseek" => Provider::deepseek(),
        "ollama" => Provider::ollama(),
        "openai" => Provider::openai(),
        "vllm" => Provider::vllm(),
        "claude-code" => Provider::claude_code(),
        "codex" => Provider::codex(),
        _ => Provider::openrouter(),
    }
}

/// API keys by provider id, as stored (`api_key_<id>`) or migrated.
#[derive(Debug, Clone, Default)]
pub struct Keys {
    map: BTreeMap<String, String>,
}

impl Keys {
    pub fn get(&self, id: &str) -> Option<String> {
        self.map.get(id).cloned().filter(|k| !k.trim().is_empty())
    }
    pub fn set(&mut self, id: &str, key: &str) {
        self.map.insert(id.into(), key.trim().into());
    }
    pub fn has(&self, id: &str) -> bool {
        self.get(id).is_some()
    }
}

/// Settings and keys, read from a settings folder.
#[derive(Debug, Clone)]
pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Store {
        Store { dir: dir.into() }
    }

    /// The user's own settings folder, or a folder in the temporary directory.
    pub fn user() -> Store {
        Store::new(settings_dir().unwrap_or_else(|| std::env::temp_dir().join("pdfflow-settings")))
    }

    /// The settings (the defaults when none are saved yet).
    pub fn load(&self) -> AgentSettings {
        std::fs::read_to_string(self.dir.join("agent_settings.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self, s: &AgentSettings) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let f = self.dir.join("agent_settings.json");
        std::fs::write(&f, serde_json::to_string_pretty(s).unwrap_or_default()).map_err(|e| format!("{}: {e}", f.display()))
    }

    /// The stored keys.
    pub fn keys(&self) -> Keys {
        let mut k = Keys::default();
        for (id, _) in PROVIDERS {
            if let Some(v) = read_key(&self.dir.join(format!("api_key_{id}"))) {
                k.set(id, &v);
            }
        }
        k
    }

    pub fn save_key(&self, id: &str, key: &str) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let p = self.dir.join(format!("api_key_{id}"));
        std::fs::write(&p, key.trim()).map_err(|e| format!("{}: {e}", p.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

fn read_key(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty())
}

/// Whether an OpenRouter model takes images (its input modalities in OpenRouter's model list),
/// asked once per model.
pub fn openrouter_takes_images(model: &str) -> Result<bool, String> {
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<BTreeMap<String, bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(b) = cache.lock().ok().and_then(|c| c.get(model).copied()) {
        return Ok(b);
    }
    let v: Value = ureq::get("https://openrouter.ai/api/v1/models").timeout(std::time::Duration::from_secs(20)).call().map_err(|e| format!("cannot reach OpenRouter: {e}"))?.into_json().map_err(|e| format!("OpenRouter model list: {e}"))?;
    let m = v["data"].as_array().and_then(|a| a.iter().find(|m| m["id"].as_str() == Some(model))).ok_or_else(|| format!("OpenRouter does not list the model '{model}'"))?;
    let b = m["architecture"]["input_modalities"].as_array().is_some_and(|a| a.iter().any(|x| x.as_str() == Some("image")));
    if let Ok(mut c) = cache.lock() {
        c.insert(model.into(), b);
    }
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pdfflow-agent-settings-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn settings_and_keys_round_trip() {
        let d = tmp("round");
        let st = Store::new(&d);
        let mut s = st.load();
        assert_eq!(s.provider, "deepseek");
        s.provider = "vllm".into();
        s.current_mut().base_url = "http://10.0.0.5:8000/v1/".into();
        s.current_mut().model = "qwen3".into();
        st.save(&s).unwrap();
        st.save_key("vllm", " token \n").unwrap();
        let back = st.load();
        assert_eq!(back, s);
        let p = back.llm_provider(&st.keys());
        assert_eq!(p.base_url, "http://10.0.0.5:8000/v1");
        assert_eq!(p.key().as_deref(), Some("token"));
        assert_eq!(back.chip(), "vLLM · qwen3");
        let _ = std::fs::remove_dir_all(&d);
    }
}
