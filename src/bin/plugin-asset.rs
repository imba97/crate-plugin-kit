//! `plugin-asset`: build a plugin checkout into the two files a release carries.
//!
//! A thin shell around [`crate_plugin_kit::pack_plugin`]: parse the arguments, make one
//! call, report what was written. Everything that decides *what* the files are called,
//! what the generated wrapper contains, and whether the result is loadable lives in the
//! library, where install-time and release-time share one implementation.
//!
//! ```text
//! plugin-asset --id pmpx
//!   -> dist/pmpx-plugin-pnpm-0.1.0-x86_64-unknown-linux-gnu.so
//!   -> dist/pmpx-plugin-pnpm-0.1.0-x86_64-unknown-linux-gnu.toml
//! ```

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate_plugin_kit::{pack_plugin, KitConfig, PackOptions};

/// Exit code for a usage error, matching the convention the hosts use (`pmpx` uses 2).
const EXIT_USAGE: u8 = 2;

const USAGE: &str = "\
plugin-asset — build a plugin checkout into the files a release carries.

Usage:
  plugin-asset --id <ID> [options]

Options:
  --id <ID>              Host application id: the same string the host passes to
                         `KitConfig::new` (for pmpx, `pmpx`). Required — the asset
                         names, the manifest name and the generated wrapper are all
                         derived from it.
  --manifest-path <PATH> The plugin's Cargo.toml. Default: ./Cargo.toml
  --out-dir <DIR>        Where the two assets go. Default: ./dist
  --target <TRIPLE>      Target triple to build for. Default: this machine's
  --target-dir <DIR>     cargo's build directory. Default: <plugin dir>/target
  --no-verify            Skip the `dlopen` check on the finished cdylib
  -h, --help             Print this help
  -V, --version          Print the version

It writes two files, named exactly as the download path expects them:

  {crate}-{version}-{target}.{so|dylib|dll}
  {crate}-{version}-{target}.toml

Both paths are printed on success.
";

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    match parse(&argv) {
        Ok(Action::Help) => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }

        Ok(Action::Version) => {
            println!("plugin-asset {}", crate_plugin_kit::VERSION);
            ExitCode::SUCCESS
        }

        Ok(Action::Pack(args)) => match run(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("plugin-asset: {message}");
                ExitCode::FAILURE
            }
        },

        Err(message) => {
            eprintln!("plugin-asset: {message}");
            eprintln!();
            eprint!("{USAGE}");
            ExitCode::from(EXIT_USAGE)
        }
    }
}

/// What the command line asked for.
enum Action {
    Help,
    Version,
    Pack(Args),
}

/// A parsed `plugin-asset` command line.
struct Args {
    id: String,
    manifest_path: PathBuf,
    out_dir: PathBuf,
    target: Option<String>,
    target_dir: Option<PathBuf>,
    verify: bool,
}

/// Runs the requested packing.
fn run(args: &Args) -> Result<(), String> {
    let mut cfg = KitConfig::new(args.id.clone());
    if let Some(target) = &args.target {
        // The same override install uses; here it also decides the asset's file name.
        cfg.target_triple = Some(target.clone());
    }

    // The plugin directory is where its Cargo.toml is; `.` for a bare file name.
    let plugin_dir = match args.manifest_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };

    let options = PackOptions {
        target_dir: args.target_dir.clone(),
        verify_export: args.verify,
    };

    let assets =
        pack_plugin(&cfg, plugin_dir, &args.out_dir, &options).map_err(|e| e.to_string())?;

    println!("{}", assets.library.display());
    println!("{}", assets.manifest.display());
    Ok(())
}

/// Parses the arguments.
///
/// Hand-rolled rather than pulled in: this is seven flags, and the crate's dependency
/// list is part of its interface — every plugin installation compiles it.
fn parse(argv: &[String]) -> Result<Action, String> {
    let mut id: Option<String> = None;
    let mut manifest_path = PathBuf::from("Cargo.toml");
    let mut out_dir = PathBuf::from("dist");
    let mut target: Option<String> = None;
    let mut target_dir: Option<PathBuf> = None;
    let mut verify = true;

    let mut args = argv.iter();
    while let Some(arg) = args.next() {
        // `--flag=value` and `--flag value` are both accepted; nothing else starts with
        // `-`, so a stray positional argument is an error rather than silently ignored.
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag, Some(value)),
            None => (arg.as_str(), None),
        };

        match flag {
            "-h" | "--help" => return Ok(Action::Help),
            "-V" | "--version" => return Ok(Action::Version),
            "--id" => id = Some(value(flag, inline, &mut args)?),
            "--manifest-path" => manifest_path = PathBuf::from(value(flag, inline, &mut args)?),
            "--out-dir" => out_dir = PathBuf::from(value(flag, inline, &mut args)?),
            "--target" => target = Some(value(flag, inline, &mut args)?),
            "--target-dir" => target_dir = Some(PathBuf::from(value(flag, inline, &mut args)?)),
            "--no-verify" => verify = false,
            other => return Err(format!("unknown argument `{other}`")),
        }
    }

    let Some(id) = id else {
        return Err(
            "--id is required: the asset names and the generated wrapper are both derived \
             from the host application id (for pmpx, `--id pmpx`)"
                .to_string(),
        );
    };

    Ok(Action::Pack(Args {
        id,
        manifest_path,
        out_dir,
        target,
        target_dir,
        verify,
    }))
}

/// The value of a flag, from `--flag=value` or from the next argument.
fn value(
    flag: &str,
    inline: Option<&str>,
    args: &mut std::slice::Iter<'_, String>,
) -> Result<String, String> {
    match inline {
        Some(value) if !value.is_empty() => Ok(value.to_string()),
        Some(_) => Err(format!("{flag} was given an empty value")),
        None => args
            .next()
            .cloned()
            .ok_or_else(|| format!("{flag} needs a value")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(argv: &[&str]) -> Args {
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        match parse(&argv).expect("should parse") {
            Action::Pack(args) => args,
            _ => panic!("expected a pack action"),
        }
    }

    fn parse_err(argv: &[&str]) -> String {
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        match parse(&argv) {
            Err(message) => message,
            Ok(_) => panic!("expected a usage error"),
        }
    }

    #[test]
    fn help_and_version_short_circuit() {
        let argv = vec!["--help".to_string()];
        assert!(matches!(parse(&argv), Ok(Action::Help)));

        let argv = vec!["-V".to_string()];
        assert!(matches!(parse(&argv), Ok(Action::Version)));
    }

    #[test]
    fn defaults_are_the_working_directory() {
        let args = parse_ok(&["--id", "pmpx"]);

        assert_eq!(args.id, "pmpx");
        assert_eq!(args.manifest_path, PathBuf::from("Cargo.toml"));
        assert_eq!(args.out_dir, PathBuf::from("dist"));
        assert_eq!(args.target, None);
        assert_eq!(args.target_dir, None);
        assert!(args.verify, "the dlopen check is on unless asked otherwise");
    }

    #[test]
    fn every_flag_can_be_set() {
        let args = parse_ok(&[
            "--id",
            "pmpx",
            "--manifest-path",
            "sub/Cargo.toml",
            "--out-dir",
            "release-assets",
            "--target",
            "aarch64-apple-darwin",
            "--target-dir",
            "cache",
            "--no-verify",
        ]);

        assert_eq!(args.manifest_path, PathBuf::from("sub/Cargo.toml"));
        assert_eq!(args.out_dir, PathBuf::from("release-assets"));
        assert_eq!(args.target.as_deref(), Some("aarch64-apple-darwin"));
        assert_eq!(args.target_dir, Some(PathBuf::from("cache")));
        assert!(!args.verify);
    }

    #[test]
    fn equals_form_is_accepted() {
        let args = parse_ok(&[
            "--id=pmpx",
            "--out-dir=dist2",
            "--target=x86_64-unknown-linux-gnu",
        ]);

        assert_eq!(args.id, "pmpx");
        assert_eq!(args.out_dir, PathBuf::from("dist2"));
        assert_eq!(args.target.as_deref(), Some("x86_64-unknown-linux-gnu"));
    }

    /// Without the id nothing can be derived, so it is a usage error rather than a guess.
    #[test]
    fn the_id_is_required() {
        let message = parse_err(&["--out-dir", "dist"]);
        assert!(message.contains("--id is required"), "{message}");
    }

    #[test]
    fn a_flag_without_a_value_is_an_error() {
        let message = parse_err(&["--id", "pmpx", "--out-dir"]);
        assert!(message.contains("--out-dir needs a value"), "{message}");
    }

    #[test]
    fn an_empty_value_is_an_error() {
        let message = parse_err(&["--id=pmpx", "--target="]);
        assert!(message.contains("empty value"), "{message}");
    }

    #[test]
    fn an_unknown_argument_is_an_error() {
        assert!(parse_err(&["--id", "pmpx", "--wat"]).contains("unknown argument"));
        assert!(parse_err(&["--id", "pmpx", "stray"]).contains("unknown argument"));
    }

    #[test]
    fn the_plugin_dir_is_the_manifests_parent() {
        let args = parse_ok(&["--id", "pmpx", "--manifest-path", "plugins/pnpm/Cargo.toml"]);
        assert_eq!(args.manifest_path.parent(), Some(Path::new("plugins/pnpm")));

        let args = parse_ok(&["--id", "pmpx"]);
        // A bare file name resolves against the current directory, which `run` turns
        // into `.` (an empty parent).
        assert_eq!(args.manifest_path.parent(), Some(Path::new("")));
    }
}
