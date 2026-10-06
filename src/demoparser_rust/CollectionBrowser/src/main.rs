mod app;
mod bottom_panel;
mod config;
mod control_panel;
mod database;
mod fetch;
mod handlers;
mod message;
mod models;
mod npz_loader;
mod playbar;
mod radar_config;
mod radar_view;
mod s2r_loader;
mod sorting;
mod style;
mod table;
mod tag_utils;
mod theme;
mod top_panel;

use app::BrowserApp;
use chrono::Local;
use iced::{Application, Settings};
use std::fs::OpenOptions;
use std::io::Write;
use std::panic;

pub fn main() -> iced::Result {
    // Set up crash logging
    panic::set_hook(Box::new(|panic_info| {
        let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
        let log_msg = format!("\n[{}] CRSASH:\n{:?}\n", timestamp, panic_info);

        let file_path = "crash_log.txt";

        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(file_path) {
            let _ = file.write_all(log_msg.as_bytes());
            eprintln!("Crash log written to {}", file_path);
        } else {
            eprintln!("Failed to write crash log: {}", log_msg);
        }
    }));

    let mut settings = Settings::default();
    settings.window.size = iced::Size::new(1280.0, 800.0);
    settings.default_font = iced::Font::with_name("Tahoma");
    settings.default_text_size = 14.0.into();

    BrowserApp::run(settings)
}
