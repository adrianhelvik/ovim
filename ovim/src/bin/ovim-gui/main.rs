#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::Result;
use ovim::cli::FileArg;

fn main() -> Result<()> {
    let mut file = None;
    let mut resume = false;
    for argument in std::env::args().skip(1) {
        if argument == "--resume" {
            resume = true;
        } else if argument.starts_with('-') {
            anyhow::bail!("Unknown GUI option: {argument}");
        } else if file.is_none() {
            file = Some(FileArg::parse(&argument));
        } else {
            anyhow::bail!("Only one file or directory can be opened at startup");
        }
    }

    let _ = ovim::log::init();
    if let Err(error) = ovim::language_config::LanguageRegistry::init() {
        ovim_core::log_warn!("gui", "Language registry initialization: {}", error);
    }
    ovim::gui::app::run(file, resume)
}
