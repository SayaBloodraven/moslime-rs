#![cfg_attr(all(feature = "gui", not(debug_assertions)), windows_subsystem = "windows")]

mod ble;
mod config;
#[cfg(feature = "gui")]
mod gui;
mod headless;
mod protocol;
mod setup;
mod tracker;
mod web;
mod winbt;

use anyhow::Result;

const HELP: &str = "\
moslime - SlimeVR bridge for Sony mocopi (QM-SS1) trackers

USAGE:
    moslime                  open the window (when built with the GUI)
    moslime --headless       no window: read config_six.json and run
    moslime --setup          (Linux) find, pair and save your pucks step by step
    moslime --list-adapters  show Bluetooth adapters and exit
    moslime --help           this text
";

fn build_runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?)
}

fn run_headless() -> Result<()> {
    let rt = build_runtime()?;
    headless::run(&rt)
}

#[cfg(feature = "gui")]
fn run_default(want_headless: bool) -> Result<()> {
    if want_headless {
        run_headless()
    } else {
        gui::run().map_err(|e| anyhow::anyhow!("{e}"))
    }
}

#[cfg(not(feature = "gui"))]
fn run_default(_want_headless: bool) -> Result<()> {
    run_headless()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);

    if has("--help") || has("-h") {
        print!("{HELP}");
        return Ok(());
    }
    if has("--setup") {
        let rt = build_runtime()?;
        return setup::run(&rt);
    }
    if has("--list-adapters") {
        let rt = build_runtime()?;
        return headless::list_adapters(&rt);
    }
    run_default(has("--headless"))
}
