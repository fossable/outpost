use anyhow::Result;
use std::{path::Path, process::ExitCode};

fn main() -> Result<ExitCode> {
    if cfg!(feature = "cloudflare") {
        let filename = format!(
            "cloudflared-{}-{}",
            std::env::consts::OS,
            match std::env::consts::ARCH {
                "x86" => "386",
                "x86_64" => "amd64",
                _ => std::env::consts::ARCH,
            }
        );

        // Download to cache
        let dest = Path::new(&std::env::var("OUT_DIR")?).join(&filename);
        println!("cargo:rustc-env=CLOUDFLARED_PATH={}", dest.display());

        let response = reqwest::blocking::get(format!(
            "https://github.com/cloudflare/cloudflared/releases/download/2024.6.0/{}",
            &filename,
        ))?;
        assert!(response.status().is_success());

        std::fs::write(dest, response.bytes()?)?;
    }

    Ok(ExitCode::SUCCESS)
}
