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
//!
//! # Layout
//!
//! `main` only turns the requested action into an exit code; what the command line can
//! say, and what each action does, is in `cli`.

mod cli;

use std::process::ExitCode;

use cli::{parse, run, Action, EXIT_USAGE, USAGE};

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
