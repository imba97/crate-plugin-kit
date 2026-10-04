//! build-host：生成一个 wrapper 工程，`cargo build` 出 cdylib，拷到安装目录。
//!
//! # 为什么需要 wrapper
//!
//! `cargo install` 只认 bin target，`crate-type = ["cdylib"]` 的 crate 装不了。
//! 而且直接 `cargo build` 一个插件 crate，Cargo 也不会替依赖产出 cdylib。
//!
//! 所以本 crate 生成一个**只有几十行**的 wrapper 工程：
//!
//! ```text
//! <build>/<crate>/
//! ├── Cargo.toml        # [lib] crate-type = ["cdylib"]，依赖插件本体 + 宿主契约 crate
//! └── src/lib.rs        # 一行：<contract>::export!(<plugin_crate>::create);
//! ```
//!
//! 好处是**插件本体的 crate 保持普通 rlib**：能被 crates.io 正常消费、
//! 能被单测直接 `use`、插件作者一行 `#[no_mangle]` 都不用写。
//!
//! # manifest 从哪来
//!
//! 插件 crate 的**源码根目录**里有一份 `<manifest_name>`。这里用 `cargo metadata`
//! 定位到 crate 源码目录，把它原样拷进安装目录 —— 见 [`crate::manifest`] 的模块文档。

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cache::InstallSource;
use crate::config::{KitConfig, KitPaths};
use crate::error::{KitError, KitResult};
use crate::install::{remove_dir_if_exists, Installed};
use crate::loader;

/// 走 build-host 装一个插件。
///
/// `version` 必须是**确定的版本号**（调用方负责先查 crates.io 拿到 latest）。
pub fn install(
    cfg: &KitConfig,
    paths: &KitPaths,
    crate_name: &str,
    version: &str,
) -> KitResult<Installed> {
    let cargo = which::which("cargo").map_err(|_| KitError::CargoNotFound)?;

    let wrapper_dir = paths.build_dir(crate_name);
    remove_dir_if_exists(&wrapper_dir)?;
    std::fs::create_dir_all(wrapper_dir.join("src"))?;

    let stem = cfg.lib_stem(crate_name);
    write_wrapper(cfg, &wrapper_dir, crate_name, version, &stem)?;

    let manifest_path = wrapper_dir.join("Cargo.toml");

    // ① metadata：解析并下载依赖，顺便拿到 target 目录 + 插件源码目录
    let metadata = cargo_metadata(&cargo, &manifest_path)?;
    let target_dir = metadata.target_directory.clone();
    let plugin_src = metadata.plugin_source_dir(crate_name);

    // ② 真正编译。stdio 继承 —— 这一步可能几分钟，用户要看得见。
    let status = Command::new(&cargo)
        .arg("build")
        .arg("--release")
        .arg("--manifest-path")
        .arg(&manifest_path)
        .status()
        .map_err(KitError::Io)?;

    if !status.success() {
        return Err(KitError::BuildFailed {
            code: status.code(),
        });
    }

    // ③ 找产物
    let release_dir = target_dir.join("release");
    let built =
        loader::find_library(&release_dir, &stem).map_err(|_| KitError::BuildArtifactMissing {
            stem: stem.clone(),
            dir: release_dir.display().to_string(),
        })?;

    // ④ 落地：cdylib + manifest
    let plugin_dir = paths.plugin_dir(crate_name);
    remove_dir_if_exists(&plugin_dir)?;
    std::fs::create_dir_all(&plugin_dir)?;

    let dest_lib = plugin_dir.join(built.file_name().expect("产物一定有文件名"));
    std::fs::copy(&built, &dest_lib)?;

    copy_manifest(cfg, &plugin_dir, plugin_src)?;

    Ok(Installed {
        crate_name: crate_name.to_string(),
        version: version.to_string(),
        dir: plugin_dir,
        library: dest_lib,
        source: InstallSource::BuildHost,
    })
}

// ---- 生成 wrapper --------------------------------------------------------

fn write_wrapper(
    cfg: &KitConfig,
    dir: &Path,
    crate_name: &str,
    version: &str,
    stem: &str,
) -> KitResult<()> {
    let crate_ident = crate_name.replace('-', "_");

    let mut toml = format!(
        r#"# 本文件由 crate-plugin-kit 生成，不要手改 —— 每次安装都会重写。
[package]
name         = "{crate_name}-wrapper"
version      = "0.0.0"
edition      = "{edition}"
publish      = false

# 单元素 workspace：wrapper 住在 <data-dir>/build/ 下，不该被任何上层 Cargo.toml 认领。
[workspace]

[lib]
name       = "{stem}"
crate-type = ["cdylib"]

[dependencies]
{crate_name} = "={version}"
{contract}   = "{contract_version}"
"#,
        crate_name = crate_name,
        edition = cfg.wrapper_edition,
        stem = stem,
        version = version,
        contract = cfg.contract_crate,
        contract_version = cfg.contract_version,
    );

    // 本地路径覆盖：开发期把插件本体 / 契约 crate 指到本地检出。
    if !cfg.local_overrides.is_empty() {
        toml.push_str("\n# 来自 KitConfig::local_overrides（开发期）\n[patch.crates-io]\n");
        for (name, path) in &cfg.local_overrides {
            let p = path.display().to_string().replace('\\', "/");
            toml.push_str(&format!("{name} = {{ path = \"{p}\" }}\n"));
        }
    }

    std::fs::write(dir.join("Cargo.toml"), toml)?;

    let body = cfg.wrapper_body.replace("{crate_ident}", &crate_ident);
    std::fs::write(dir.join("src").join("lib.rs"), body)?;

    // 刻意**不**给 wrapper 写 `rust-toolchain.toml`。
    //
    // wrapper 住在 `<data-dir>/build/` 下，那里向上找不到任何工具链文件，
    // 所以 cargo 会用当前默认工具链 —— 这正是想要的：**用你本来就在用的那一套**。
    // 写死一个 channel 反而会在安装时触发一次意料之外的 rustup 下载。
    //
    // 何况宿主与插件本来就不需要同一个工具链（跨边界传的只有 `#[repr(C)]` 数据），
    // 这里也没有"必须钉住"的理由。

    Ok(())
}

/// 把插件源码目录里的 manifest 拷进安装目录。
fn copy_manifest(cfg: &KitConfig, plugin_dir: &Path, plugin_src: Option<&Path>) -> KitResult<()> {
    let Some(src_dir) = plugin_src else {
        return Err(KitError::ManifestMissingField {
            field: cfg.manifest_name.clone(),
            path: PathBuf::from("<插件源码目录未定位到>"),
        });
    };

    let src = src_dir.join(&cfg.manifest_name);
    if !src.is_file() {
        return Err(KitError::ManifestMissingField {
            field: cfg.manifest_name.clone(),
            path: src,
        });
    }

    std::fs::copy(&src, plugin_dir.join(&cfg.manifest_name))?;
    Ok(())
}

// ---- cargo metadata ------------------------------------------------------

/// 从 `cargo metadata` 里挑我们要的几个值。
struct Metadata {
    target_directory: PathBuf,
    /// crate 名 → 源码目录。
    packages: Vec<(String, PathBuf)>,
}

impl Metadata {
    fn plugin_source_dir(&self, crate_name: &str) -> Option<&Path> {
        self.packages
            .iter()
            .find(|(name, _)| name == crate_name)
            .map(|(_, dir)| dir.as_path())
    }
}

fn cargo_metadata(cargo: &Path, manifest_path: &Path) -> KitResult<Metadata> {
    let out = Command::new(cargo)
        .arg("metadata")
        .arg("--format-version")
        .arg("1")
        .arg("--manifest-path")
        .arg(manifest_path)
        .output()
        .map_err(KitError::Io)?;

    if !out.status.success() {
        return Err(KitError::BuildFailed {
            code: out.status.code(),
        });
    }

    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| KitError::Registry(format!("cargo metadata 输出解析失败：{e}")))?;

    let target_directory = v
        .get("target_directory")
        .and_then(|x| x.as_str())
        .map(PathBuf::from)
        .ok_or_else(|| KitError::Registry("cargo metadata 没有 target_directory".into()))?;

    let mut packages = Vec::new();
    if let Some(arr) = v.get("packages").and_then(|x| x.as_array()) {
        for p in arr {
            let (Some(name), Some(mp)) = (
                p.get("name").and_then(|x| x.as_str()),
                p.get("manifest_path").and_then(|x| x.as_str()),
            ) else {
                continue;
            };
            // manifest_path 是 .../<crate>/Cargo.toml，父目录就是源码目录
            if let Some(dir) = Path::new(mp).parent() {
                packages.push((name.to_string(), dir.to_path_buf()));
            }
        }
    }

    Ok(Metadata {
        target_directory,
        packages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::time::Duration;

    fn cfg() -> KitConfig {
        let mut c = KitConfig::new("myapp");
        c.contract_version = "0.1".to_string();
        c.wrapper_body = "myapp_plugin::export!({crate_ident}::create);\n".to_string();
        c.lock_timeout = Duration::from_millis(500);
        c
    }

    fn generate(c: &KitConfig) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("build").join("myapp-plugin-foo");
        std::fs::create_dir_all(dir.join("src")).unwrap();

        let stem = c.lib_stem("myapp-plugin-foo");
        write_wrapper(c, &dir, "myapp-plugin-foo", "0.1.0", &stem).unwrap();
        (tmp, dir)
    }

    #[test]
    fn wrapper_manifest_names_the_right_library() {
        let c = cfg();
        let (_tmp, dir) = generate(&c);
        let toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();

        assert!(
            toml.contains(r#"name       = "myapp_plugin_foo""#),
            "{toml}"
        );
        assert!(toml.contains(r#"crate-type = ["cdylib"]"#), "{toml}");
    }

    #[test]
    fn wrapper_is_not_publishable() {
        let c = cfg();
        let (_tmp, dir) = generate(&c);
        let toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();

        assert!(toml.contains("publish      = false"), "{toml}");
    }

    /// wrapper 是单元素 workspace —— 它住在 `<data-dir>/build/` 下，
    /// 不该被任何上层 `Cargo.toml` 认领。
    #[test]
    fn wrapper_is_its_own_workspace() {
        let c = cfg();
        let (_tmp, dir) = generate(&c);
        let toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();

        assert!(toml.contains("[workspace]"), "{toml}");
    }

    /// 版本要精确锁定，否则 `.plugins.json` 里记的版本和编出来的 cdylib 可能对不上。
    #[test]
    fn the_plugin_version_is_pinned_exactly() {
        let c = cfg();
        let (_tmp, dir) = generate(&c);
        let toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();

        assert!(toml.contains(r#"myapp-plugin-foo = "=0.1.0""#), "{toml}");
    }

    #[test]
    fn contract_crate_is_a_dependency() {
        let c = cfg();
        let (_tmp, dir) = generate(&c);
        let toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();

        assert!(toml.contains("myapp-plugin"), "{toml}");
        assert!(toml.contains(r#""0.1""#), "{toml}");
    }

    #[test]
    fn body_substitutes_the_crate_ident() {
        let c = cfg();
        let (_tmp, dir) = generate(&c);
        let body = std::fs::read_to_string(dir.join("src").join("lib.rs")).unwrap();

        // 短横线要变成下划线，否则不是合法的 Rust 路径
        assert_eq!(body, "myapp_plugin::export!(myapp_plugin_foo::create);\n");
        assert!(!body.contains("{crate_ident}"), "占位符没被替换：{body}");
    }

    #[test]
    fn body_substitution_handles_multi_hyphen_names() {
        let c = cfg();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("w");
        std::fs::create_dir_all(dir.join("src")).unwrap();

        let stem = c.lib_stem("myapp-plugin-a-b");
        write_wrapper(&c, &dir, "myapp-plugin-a-b", "0.1.0", &stem).unwrap();

        let body = std::fs::read_to_string(dir.join("src").join("lib.rs")).unwrap();
        assert_eq!(body, "myapp_plugin::export!(myapp_plugin_a_b::create);\n");
    }

    #[test]
    fn writes_a_body_but_no_toolchain_pin() {
        let c = cfg();
        let (_tmp, dir) = generate(&c);

        assert!(dir.join("src").join("lib.rs").is_file());
        // 不给 wrapper 写工具链文件 —— 见 `write_wrapper` 里的注释
        assert!(
            !dir.join("rust-toolchain.toml").exists(),
            "wrapper 不该钉工具链"
        );
    }

    #[test]
    fn no_patch_section_without_local_overrides() {
        let c = cfg();
        let (_tmp, dir) = generate(&c);
        let toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();

        assert!(!toml.contains("[patch.crates-io]"), "{toml}");
    }

    /// 开发期本地联调：宿主显式给的路径要进 wrapper 的 `[patch.crates-io]`。
    /// wrapper 看不见宿主项目的 `.cargo/config.toml`，所以只能这样传。
    #[test]
    fn local_overrides_become_a_patch_section() {
        let mut c = cfg();
        c.local_overrides = BTreeMap::from([
            (
                "myapp-plugin-foo".to_string(),
                PathBuf::from("/local/plugin-foo"),
            ),
            ("myapp-plugin".to_string(), PathBuf::from("/local/contract")),
        ]);

        let (_tmp, dir) = generate(&c);
        let toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();

        assert!(toml.contains("[patch.crates-io]"), "{toml}");
        assert!(
            toml.contains(r#"myapp-plugin-foo = { path = "/local/plugin-foo" }"#),
            "{toml}"
        );
        assert!(
            toml.contains(r#"myapp-plugin = { path = "/local/contract" }"#),
            "{toml}"
        );
    }

    /// Windows 路径要写成正斜杠，否则 TOML 里的反斜杠会被当转义。
    #[test]
    fn local_override_paths_use_forward_slashes() {
        let mut c = cfg();
        c.local_overrides = BTreeMap::from([(
            "myapp-plugin-foo".to_string(),
            PathBuf::from(r"D:\local\plugin-foo"),
        )]);

        let (_tmp, dir) = generate(&c);
        let toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();

        assert!(toml.contains(r#"path = "D:/local/plugin-foo""#), "{toml}");
        // 反斜杠一个都不该剩下（会被 TOML 当转义序列）
        let patch_line = toml
            .lines()
            .find(|l| l.starts_with("myapp-plugin-foo"))
            .unwrap();
        assert!(!patch_line.contains('\\'), "{patch_line}");
    }

    #[test]
    fn plugin_source_dir_is_looked_up_by_name() {
        let meta = Metadata {
            target_directory: PathBuf::from("/target"),
            packages: vec![
                ("other".to_string(), PathBuf::from("/src/other")),
                (
                    "myapp-plugin-foo".to_string(),
                    PathBuf::from("/src/myapp-plugin-foo"),
                ),
            ],
        };

        assert_eq!(
            meta.plugin_source_dir("myapp-plugin-foo"),
            Some(Path::new("/src/myapp-plugin-foo"))
        );
        assert_eq!(meta.plugin_source_dir("absent"), None);
    }
}
