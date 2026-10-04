//! prebuilt：从 GitHub Releases 直接下载 cdylib + manifest。
//!
//! 这是**加速路径**，不是默认路径。因为它走的是 GitHub，会绕开用户已经配好的
//! cargo registry / 镜像 —— 国内环境往往连不上。所以：
//!
//! - 默认仍然是 build-host（走 `cargo build`，天然吃镜像配置）；
//! - prebuilt 只在前者被显式关闭、或调用方想"先试试能不能省几分钟"时用；
//! - 任何一步拿到 `None` 都表示"没有可用的 prebuilt"，调用方**必须回落 build-host**。
//!
//! # 产物命名约定
//!
//! Release 里放**两个裸文件**（不是压缩包，省掉 tar/gzip/zip 三个依赖）：
//!
//! ```text
//! {crate}-{version}-{target}.{so|dylib|dll}   ← cdylib 本体
//! {crate}-{version}-{target}.toml             ← manifest
//! ```
//!
//! 例：`bmux-plugin-cargo-0.1.0-x86_64-pc-windows-msvc.dll`
//!
//! 仓库地址从 **crates.io 的 `repository` 字段**取 —— 因为此时插件还没装上，
//! 我们唯一能问的就是 registry。

use std::path::PathBuf;

use crate::cache::InstallSource;
use crate::config::{KitConfig, KitPaths};
use crate::error::{KitError, KitResult};
use crate::install::{remove_dir_if_exists, Installed};
use crate::manifest::PluginManifest;
use crate::registry::Registry;

/// 单个产物的大小上限。防着"下到一个几百 MB 的东西"。
const MAX_ASSET_BYTES: u64 = 128 * 1024 * 1024;

/// 试装 prebuilt。
///
/// - `Ok(Some(_))`：装好了。
/// - `Ok(None)`：没有可用的 prebuilt（没发、或者资产缺失），**调用方应回落 build-host**。
/// - `Err(_)`：网络之类的硬错误。调用方**仍然可以**选择回落，但值得先把错误透出去。
pub fn try_install(
    cfg: &KitConfig,
    paths: &KitPaths,
    registry: &Registry,
    crate_name: &str,
    version: &str,
) -> KitResult<Option<Installed>> {
    // ① 仓库地址只能问 registry
    let Some(info) = registry.view(crate_name)? else {
        return Ok(None);
    };
    let Some(repo) = info.repository.as_deref() else {
        return Ok(None);
    };
    let Some((owner, name)) = parse_github_repo(repo) else {
        // 不是 GitHub 就没有我们能猜的 Release URL
        return Ok(None);
    };

    let target = cfg.effective_target();
    let ext = platform_lib_ext();
    let base = format!("https://github.com/{owner}/{name}/releases/download/v{version}/{crate_name}-{version}-{target}");

    let lib_url = format!("{base}.{ext}");
    let manifest_url = format!("{base}.toml");

    // ② 先取 cdylib。404 就是"没发 prebuilt"。
    let Some(lib_bytes) = registry.try_download(&lib_url, MAX_ASSET_BYTES)? else {
        return Ok(None);
    };

    // ③ manifest 必须一起发出来。缺了它这个 prebuilt 不可用 ——
    //    没有 manifest 就没法检测，装了也白装。
    let Some(manifest_bytes) = registry.try_download(&manifest_url, MAX_ASSET_BYTES)? else {
        return Ok(None);
    };

    // ④ 内容要自洽：manifest 里写的名字和版本必须就是我们下的那个
    let manifest_text = String::from_utf8(manifest_bytes)
        .map_err(|e| KitError::Registry(format!("{manifest_url} 不是合法 UTF-8：{e}")))?;
    let manifest = PluginManifest::parse(&manifest_text, &PathBuf::from(&manifest_url))?;

    if manifest.plugin.version != version {
        return Err(KitError::Registry(format!(
            "prebuilt manifest 版本不符：资产声明 {}，要装的是 {version}",
            manifest.plugin.version
        )));
    }

    // ⑤ 落地
    let plugin_dir = paths.plugin_dir(crate_name);
    remove_dir_if_exists(&plugin_dir)?;
    std::fs::create_dir_all(&plugin_dir)?;

    let lib_name = format!("{}.{ext}", manifest.effective_lib_stem(cfg, crate_name));
    let dest_lib = plugin_dir.join(&lib_name);
    std::fs::write(&dest_lib, lib_bytes)?;

    manifest.write(&paths.manifest_path(crate_name, &cfg.manifest_name))?;

    Ok(Some(Installed {
        crate_name: crate_name.to_string(),
        version: version.to_string(),
        dir: plugin_dir,
        library: dest_lib,
        source: InstallSource::Prebuilt,
    }))
}

/// 当前平台的 cdylib 扩展名。
fn platform_lib_ext() -> &'static str {
    match std::env::consts::OS {
        "windows" => "dll",
        "macos" => "dylib",
        _ => "so",
    }
}

/// 从 GitHub URL 里取出 `(owner, repo)`。
///
/// 认这些形态：
///
/// ```text
/// https://github.com/owner/repo
/// https://github.com/owner/repo.git
/// https://github.com/owner/repo/
/// git+https://github.com/owner/repo
/// ```
pub fn parse_github_repo(url: &str) -> Option<(String, String)> {
    let rest = url
        .trim()
        .strip_prefix("git+")
        .unwrap_or(url)
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))?;

    let mut parts = rest.trim_end_matches('/').split('/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim().trim_end_matches(".git");

    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

#[cfg(test)]
mod tests {
    use super::parse_github_repo;

    #[test]
    fn parses_common_forms() {
        let want = Some(("imba97".to_string(), "bmux-plugin-cargo".to_string()));
        assert_eq!(
            parse_github_repo("https://github.com/imba97/bmux-plugin-cargo"),
            want
        );
        assert_eq!(
            parse_github_repo("https://github.com/imba97/bmux-plugin-cargo.git"),
            want
        );
        assert_eq!(
            parse_github_repo("https://github.com/imba97/bmux-plugin-cargo/"),
            want
        );
        assert_eq!(
            parse_github_repo("git+https://github.com/imba97/bmux-plugin-cargo"),
            want
        );
    }

    #[test]
    fn rejects_non_github() {
        assert_eq!(parse_github_repo("https://gitlab.com/a/b"), None);
        assert_eq!(parse_github_repo("https://github.com/onlyowner"), None);
    }
}
