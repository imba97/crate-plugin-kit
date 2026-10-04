//! crates.io 查询。
//!
//! 只用两个接口：
//!
//! | 用途 | 接口 |
//! | ---- | ---- |
//! | 按关键词搜 | `GET {registry}/api/v1/crates?q=<kw>&per_page=N` |
//! | 按精确名查 | `GET {registry}/api/v1/crates/<name>` |
//!
//! `registry` 可以指向镜像（见 [`crate::KitConfig::registry`]）。
//!
//! crates.io **要求带 `User-Agent`**，不带会被 403。这里用宿主 id + 本 crate 版本拼。

use serde::Deserialize;

use crate::config::KitConfig;
use crate::error::{KitError, KitResult};

/// 一条搜索结果。
#[derive(Debug, Clone)]
pub struct CrateSummary {
    /// crate 名。
    pub name: String,
    /// 最新版本。
    pub version: String,
    /// 描述。
    pub description: Option<String>,
    /// 总下载量。
    pub downloads: u64,
}

/// 某个 crate 的详细信息。
#[derive(Debug, Clone)]
pub struct CrateInfo {
    /// crate 名。
    pub name: String,
    /// 最新版本。
    pub version: String,
    /// 描述。
    pub description: Option<String>,
    /// 源码仓库地址（prebuilt 下载要靠它拼 Release URL）。
    pub repository: Option<String>,
}

/// crates.io 客户端。
#[derive(Debug, Clone)]
pub struct Registry {
    base: String,
    user_agent: String,
}

// ---- 线上返回的 JSON 形状 ------------------------------------------------

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
    /// 按配置建一个客户端。
    pub fn new(cfg: &KitConfig) -> Self {
        Self {
            base: cfg.registry.trim_end_matches('/').to_string(),
            // crates.io 要求 User-Agent 里带联系方式，否则可能被限流。
            // 仓库地址取自 `Cargo.toml` 的 `repository` 字段（Cargo 编译期注入），
            // 这样它不会和 manifest 脱节。
            user_agent: format!(
                "{} (crate-plugin-kit/{}; +{})",
                cfg.id,
                env!("CARGO_PKG_VERSION"),
                env!("CARGO_PKG_REPOSITORY"),
            ),
        }
    }

    /// 按关键词搜索。
    pub fn search(&self, keyword: &str, limit: usize) -> KitResult<Vec<CrateSummary>> {
        let url = format!(
            "{}/api/v1/crates?q={}&per_page={}",
            self.base,
            percent_encode(keyword),
            limit
        );
        let text = self.get_text(&url)?;

        let parsed: SearchResponse = serde_json::from_str(&text)
            .map_err(|e| KitError::Registry(format!("搜索结果解析失败：{e}")))?;

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

    /// 按精确名查询。不存在返回 `Ok(None)`。
    pub fn view(&self, name: &str) -> KitResult<Option<CrateInfo>> {
        let url = format!("{}/api/v1/crates/{}", self.base, percent_encode(name));

        let text = match self.get_text(&url) {
            Ok(t) => t,
            // 404 是"这个 crate 不存在"，不是错误。
            Err(KitError::Http(msg)) if msg.contains("404") => return Ok(None),
            Err(e) => return Err(e),
        };

        let parsed: ViewResponse = serde_json::from_str(&text)
            .map_err(|e| KitError::Registry(format!("查询结果解析失败：{e}")))?;

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
            .map_err(|e| KitError::Http(format!("读取响应体失败（{url}）：{e}")))
    }

    /// 下载一个文件到内存。**用不到就返回 `Ok(None)`**（404 等）。
    ///
    /// prebuilt 路径用它取单个产物文件。
    pub fn try_download(&self, url: &str, limit_bytes: u64) -> KitResult<Option<Vec<u8>>> {
        match ureq::get(url).header("User-Agent", &self.user_agent).call() {
            Ok(mut res) => {
                let body = res
                    .body_mut()
                    .read_to_vec()
                    .map_err(|e| KitError::Http(format!("读取 {url} 失败：{e}")))?;
                if body.len() as u64 > limit_bytes {
                    return Err(KitError::Http(format!(
                        "{url} 超过大小上限（{} 字节 > {limit_bytes}）",
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

/// 把 ureq 的错误变成一句人能读的话（把状态码单独拎出来，调用方要判断 404）。
fn describe_ureq_error(e: &ureq::Error, url: &str) -> String {
    match e {
        ureq::Error::StatusCode(code) => format!("HTTP {code}（{url}）"),
        other => format!("{other}（{url}）"),
    }
}

/// 最小百分号编码：只放过 URL 里安全的字符。
///
/// 不引 `urlencoding` 之类的小依赖，就为这一个函数 —— crate 名和搜索词都不长。
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
