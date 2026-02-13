use anyhow::{Context, Result};
use std::{env, path::PathBuf};

/// Default server port when PORT is not set.
const DEFAULT_PORT: u16 = 3000;

/// Environment variable names for upstream and config.
pub mod env_keys {
    pub const PORT: &str = "PORT";
    pub const UPSTREAM_BASE_URL: &str = "UPSTREAM_BASE_URL";
    pub const ANTHROPIC_PROXY_BASE_URL: &str = "ANTHROPIC_PROXY_BASE_URL";
    pub const UPSTREAM_API_KEY: &str = "UPSTREAM_API_KEY";
    pub const OPENROUTER_API_KEY: &str = "OPENROUTER_API_KEY";
    pub const REASONING_MODEL: &str = "REASONING_MODEL";
    pub const COMPLETION_MODEL: &str = "COMPLETION_MODEL";
    pub const DEBUG: &str = "DEBUG";
    pub const VERBOSE: &str = "VERBOSE";
}

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub base_url: String,
    /// Cached URL for upstream chat completions (avoids format! on every request).
    pub(crate) chat_completions_url: String,
    pub api_key: Option<String>,
    pub reasoning_model: Option<String>,
    pub completion_model: Option<String>,
    pub debug: bool,
    pub verbose: bool,
}

impl Config {
    /// Try to load .env from the given path; then from cwd, home, and /etc.
    fn load_dotenv(custom_path: Option<PathBuf>) -> Option<PathBuf> {
        if let Some(path) = custom_path {
            if path.exists() && dotenvy::from_path(&path).is_ok() {
                return Some(path);
            }
            eprintln!("WARNING: Custom config file not found: {}", path.display());
        }

        if let Ok(path) = dotenvy::dotenv() {
            return Some(path);
        }

        let home = env::var("HOME")
            .ok()
            .or_else(|| env::var("USERPROFILE").ok());
        if let Some(home) = home {
            let home_config = PathBuf::from(&home).join(".anthropic-proxy.env");
            if home_config.exists() && dotenvy::from_path(&home_config).is_ok() {
                return Some(home_config);
            }
        }

        let etc_config = PathBuf::from("/etc/anthropic-proxy/.env");
        if etc_config.exists() && dotenvy::from_path(&etc_config).is_ok() {
            return Some(etc_config);
        }

        None
    }

    /// Parse an env var as a boolean (true, 1, yes => true).
    fn env_bool(key: &str) -> bool {
        env::var(key)
            .map(|v| {
                let v = v.to_lowercase();
                v == "1" || v == "true" || v == "yes"
            })
            .unwrap_or(false)
    }

    pub fn from_env() -> Result<Self> {
        Self::from_env_with_path(None)
    }

    pub fn from_env_with_path(custom_path: Option<PathBuf>) -> Result<Self> {
        use env_keys::*;

        if let Some(path) = Self::load_dotenv(custom_path) {
            eprintln!("Loaded config from: {}", path.display());
        } else {
            eprintln!("No .env file found, using environment variables only");
        }

        let port = env::var(PORT)
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(DEFAULT_PORT);

        let raw_base_url = env::var(UPSTREAM_BASE_URL)
            .or_else(|_| env::var(ANTHROPIC_PROXY_BASE_URL))
            .context(
                "UPSTREAM_BASE_URL is required. Set it to your OpenAI-compatible endpoint (e.g. \
                 https://openrouter.ai/api, https://api.openai.com, http://localhost:11434)",
            )?;

        let base_url = raw_base_url.trim().trim_end_matches('/').to_string();
        reqwest::Url::parse(&base_url).context("UPSTREAM_BASE_URL must be a valid URL")?;

        if base_url.ends_with("/v1") {
            eprintln!(
                "WARNING: UPSTREAM_BASE_URL ends with '/v1'. The proxy adds /v1/chat/completions \
                 itself. Prefer e.g. https://openrouter.ai/api (without /v1)."
            );
        }

        let api_key = env::var(UPSTREAM_API_KEY)
            .or_else(|_| env::var(OPENROUTER_API_KEY))
            .ok()
            .filter(|k| !k.is_empty());

        let reasoning_model = env::var(REASONING_MODEL).ok();
        let completion_model = env::var(COMPLETION_MODEL).ok();
        let debug = Self::env_bool(DEBUG);
        let verbose = Self::env_bool(VERBOSE);

        let chat_completions_url = format!("{}/v1/chat/completions", base_url);

        Ok(Config {
            port,
            base_url,
            chat_completions_url,
            api_key,
            reasoning_model,
            completion_model,
            debug,
            verbose,
        })
    }

    /// URL for the upstream chat completions endpoint.
    #[inline]
    pub fn chat_completions_url(&self) -> &str {
        &self.chat_completions_url
    }
}
