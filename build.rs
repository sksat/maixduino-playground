//! Bridges WiFi credentials from a plain `.env` file into the build, so they live
//! in `wifi_creds.env` (gitignored, never committed) instead of a source file.
//!
//! Copy `wifi_creds.env.example` to `wifi_creds.env` and fill it in; this script
//! reads it and passes the values to the compiler via `cargo:rustc-env`, which
//! `src/main.rs` picks up with `env!("WIFI_SSID")` / `env!("WIFI_PASS")`. The
//! values are consumed by cargo, not printed in normal build output.

use std::fs;

fn main() {
    let path = "wifi_creds.env";
    println!("cargo:rerun-if-changed={path}");

    let mut ssid = String::new();
    let mut pass = String::new();
    match fs::read_to_string(path) {
        Ok(text) => {
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((k, v)) = line.split_once('=') {
                    let v = v.trim().trim_matches('"');
                    match k.trim() {
                        "WIFI_SSID" => ssid = v.to_string(),
                        "WIFI_PASS" => pass = v.to_string(),
                        _ => {}
                    }
                }
            }
        }
        Err(_) => {
            println!(
                "cargo:warning=wifi_creds.env not found; building with empty WiFi \
                 creds. Copy wifi_creds.env.example to wifi_creds.env and fill it in."
            );
        }
    }

    // Consumed by cargo (not echoed in a normal build); read by env! in main.rs.
    println!("cargo:rustc-env=WIFI_SSID={ssid}");
    println!("cargo:rustc-env=WIFI_PASS={pass}");
}
