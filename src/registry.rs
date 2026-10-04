//! crates.io queries.
//!
//! Only two endpoints are used:
//!
//! | Purpose | Endpoint |
//! | ---- | ---- |
//! | Search by keyword | `GET {registry}/api/v1/crates?q=<kw>&per_page=N` |
//! | Look up by exact name | `GET {registry}/api/v1/crates/<name>` |
//!
//! `registry` may point at a mirror (see [`crate::KitConfig::registry`]).
//!
//! crates.io requires a `User-Agent` and answers 403 without one. It is built from
//! the host id plus this crate's version.

use serde::Deserialize;

use crate::config::KitConfig;
use crate::error::{KitError, KitResult};

/// One search result.
#[derive(Debug, Clone)]
pub struct CrateSummary {
    /// Crate name.
    pub name: String,
    /// Latest version.
    pub version: String,
    /// Description.
    pub description: Option<String>,
    /// Total download count.
    pub downloads: u64,
}

/// Detailed information about one crate.
#[derive(Debug, Clone)]
pub struct CrateInfo {
    /// Crate name.
    pub name: String,
    /// Latest version.
    pub version: String,
    /// Description.
    pub description: Option<String>,
    /// Source repository URL (prebuilt downloads build the Release URL from it).
    pub repository: Option<String>,
}

/// crates.io client.
#[derive(Debug, Clone)]
pub struct Registry {
    base: String,
    user_agent: String,
}

// ---- JSON shapes returned by the API ---------------------------------------

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    crates: Vec<SearchCrate>,
}

#[derive(Deserialize)]
struct SearchCrate {
    name: String,
    #[serde(default)]
    max_version: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    downloads: u64,
}

#[derive(Deserialize)]
struct ViewResponse {
    #[serde(rename = "crate")]
    krate: ViewCrate,
}

#[derive(Deserialize)]
struct ViewCrate {
    name: String,
    #[serde(default)]
    max_version: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    repository: Option<String>,
}

impl Registry {
    /// Builds a client from the config.
    pub fn new(cfg: &KitConfig) -> Self {
        Self {
            base: cfg.registry.trim_end_matches('/').to_string(),
            // crates.io wants contact information in the User-Agent, otherwise the
            // requests may be rate-limited. The repository URL comes from
            // `Cargo.toml`'s `repository` field (injected by Cargo at compile time)
            // so that it cannot drift away from the manifest.
            user_agent: format!(
                "{} (crate-plugin-kit/{}; +{})",
                cfg.id,
                env!("CARGO_PKG_VERSION"),
                env!("CARGO_PKG_REPOSITORY"),
            ),
        }
    }

    /// Searches by keyword.
    pub fn search(&self, keyword: &str, limit: usize) -> KitResult<Vec<CrateSummary>> {
        let url = format!(
            "{}/api/v1/crates?q={}&per_page={}",
            self.base,
            percent_encode(keyword),
            limit
        );
        let text = self.get_text(&url)?;

        let parsed: SearchResponse = serde_json::from_str(&text)
            .map_err(|e| KitError::Registry(format!("failed to parse search results: {e}")))?;

        Ok(parsed
            .crates
            .into_iter()
            .map(|c| CrateSummary {
                name: c.name,
                version: c.max_version.unwrap_or_default(),
                description: c.description,
                downloads: c.downloads,
            })
            .collect())
    }

    /// Looks up an exact name. Returns `Ok(None)` when it does not exist.
    pub fn view(&self, name: &str) -> KitResult<Option<CrateInfo>> {
        let url = format!("{}/api/v1/crates/{}", self.base, percent_encode(name));

        let text = match self.get_text(&url) {
            Ok(t) => t,
            // A 404 means "this crate does not exist", which is not an error.
            Err(KitError::Http(msg)) if msg.contains("404") => return Ok(None),
            Err(e) => return Err(e),
        };

        let parsed: ViewResponse = serde_json::from_str(&text)
            .map_err(|e| KitError::Registry(format!("failed to parse lookup result: {e}")))?;

        Ok(Some(CrateInfo {
            name: parsed.krate.name,
            version: parsed.krate.max_version.unwrap_or_default(),
            description: parsed.krate.description,
            repository: parsed.krate.repository,
        }))
    }

    fn get_text(&self, url: &str) -> KitResult<String> {
        let mut res = ureq::get(url)
            .header("User-Agent", &self.user_agent)
            .header("Accept", "application/json")
            .call()
            .map_err(|e| KitError::Http(describe_ureq_error(&e, url)))?;

        res.body_mut()
            .read_to_string()
            .map_err(|e| KitError::Http(format!("failed to read response body ({url}): {e}")))
    }

    /// Downloads one file into memory. Returns `Ok(None)` when there is nothing to
    /// download (a 404, for instance).
    ///
    /// The prebuilt path uses it to fetch a single artifact file.
    pub fn try_download(&self, url: &str, limit_bytes: u64) -> KitResult<Option<Vec<u8>>> {
        match ureq::get(url).header("User-Agent", &self.user_agent).call() {
            Ok(mut res) => {
                let body = res
                    .body_mut()
                    .read_to_vec()
                    .map_err(|e| KitError::Http(format!("failed to read {url}: {e}")))?;
                if body.len() as u64 > limit_bytes {
                    return Err(KitError::Http(format!(
                        "{url} exceeds the size limit ({} bytes > {limit_bytes})",
                        body.len()
                    )));
                }
                Ok(Some(body))
            }
            Err(ureq::Error::StatusCode(404)) => Ok(None),
            Err(e) => Err(KitError::Http(describe_ureq_error(&e, url))),
        }
    }
}

/// Turns a ureq error into a sentence a human can read, with the status code pulled
/// out on its own because callers need to detect 404.
fn describe_ureq_error(e: &ureq::Error, url: &str) -> String {
    match e {
        ureq::Error::StatusCode(code) => format!("HTTP {code} ({url})"),
        other => format!("{other} ({url})"),
    }
}

/// Minimal percent encoding: only the characters that are safe in a URL are left
/// alone.
///
/// A dependency like `urlencoding` is not worth pulling in for this one function —
/// crate names and search terms are short.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::percent_encode;

    #[test]
    fn encodes_reserved_characters() {
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(percent_encode("bmux-plugin"), "bmux-plugin");
        assert_eq!(percent_encode("a/b?c"), "a%2Fb%3Fc");
    }
}
