// Copyright (C) 2017-2026 Smart Code OOD 203358507

//! Build script.
//!
//! This Torbox-backed fork no longer ships `server.js`, `stremio-runtime`,
//! `ffmpeg` or `ffprobe`, so the historical "download dl.strem.io blobs"
//! step has been removed. The only remaining build-time concern is the
//! Windows resource (icon + version info), which is handled below.

use std::env::consts::OS;
use std::error::Error;

#[cfg(target_os = "windows")]
use chrono::{Datelike, Local};

const SUPPORTED_OS: &[&str] = &["linux", "macos", "windows"];

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=src/");

    if !SUPPORTED_OS.contains(&OS) {
        panic!("OS {OS} not supported, supported OSes are: {SUPPORTED_OS:?}",)
    }

    #[cfg(target_os = "windows")]
    {
        let current_dir = std::env::current_dir()?;
        let resources = current_dir.join("resources");

        let now = Local::now();
        let copyright = format!("Copyright © {} Smart Code OOD", now.year());
        let description =
            std::env::var("CARGO_PKG_DESCRIPTION").expect("Failed to read package description");

        let icon_path = resources.join("service.ico");
        let icon = icon_path.to_str().expect("Failed to find icon");

        let mut res = winres::WindowsResource::new();
        res.set("FileDescription", &description);
        res.set("LegalCopyright", &copyright);
        res.set_icon_with_id(icon, "ICON");
        res.compile().unwrap();
    }

    Ok(())
}
