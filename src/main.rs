use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use ads_simulator::generate_marketplace;
use tempfile::NamedTempFile;

const USAGE: &str = "Usage: ads-simulator --output-dir DIRECTORY [--seed INTEGER]";

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let mut output_dir = None;
    let mut seed = 42;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("-h" | "--help") => {
                println!("{USAGE}");
                return Ok(());
            }
            Some("--output-dir") => {
                output_dir = Some(PathBuf::from(value(&mut args, "--output-dir")?));
            }
            Some("--seed") => {
                let seed_arg = value(&mut args, "--seed")?;
                seed = seed_arg
                    .to_str()
                    .and_then(|text| text.parse::<i64>().ok())
                    .ok_or("--seed expects a signed 64-bit integer")?;
            }
            _ => {
                return Err(format!("Unknown argument: {}\n{USAGE}", arg.to_string_lossy()).into());
            }
        }
    }
    let output_dir = output_dir.ok_or(USAGE)?;
    let output = write_marketplace(&output_dir, seed)?;
    println!("{}", output.display());
    Ok(())
}

fn value(args: &mut impl Iterator<Item = OsString>, flag: &str) -> Result<OsString, String> {
    args.next()
        .ok_or_else(|| format!("Missing value for {flag}\n{USAGE}"))
}

fn write_marketplace(output_dir: &Path, seed: i64) -> Result<PathBuf, Box<dyn Error>> {
    fs::create_dir_all(output_dir)?;
    let output = output_dir.join("marketplace.json");
    let mut temporary = NamedTempFile::new_in(output_dir)?;
    let mut writer = BufWriter::new(temporary.as_file_mut());
    serde_json::to_writer(&mut writer, &generate_marketplace(seed))?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    drop(writer);
    temporary.persist(&output)?;
    Ok(output)
}
