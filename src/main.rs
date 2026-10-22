use anyhow::{Result, bail};
use std::path::Path;

fn main() {
    if let Err(error) = main_result() {
        eprintln!("USNJMon: {error:#}");
        std::process::exit(1);
    }
}

fn main_result() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.is_empty() || args[0] == "--help" {
        println!(
            "USNJMon {}\n\
            Convert an extracted $UsnJrnl:$J file (no installation required):\n  \
            usnjmon convert <input.usnj> <output.evtx>\n\n\
            Continuous collection (install with scripts\\Install.ps1):\n  \
            usnjmon service <config.json> <state-directory>\n\n\
            Administration:\n  usnjmon foreground <config.json> <state-directory>\n  \
            usnjmon verify-resources <usnjmon-provider.dll>\n  usnjmon check-config <config.json>\n\n\
            Conversion writes only the specified EVTX; existing output is never overwritten.\n\
            Service/foreground modes write USNJMon/Operational. Journals are never modified.",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }
    if args[0] == "check-config" && args.len() == 2 {
        usnjmon::config::Config::load(Path::new(&args[1]))?;
        println!("Configuration valid.");
        return Ok(());
    }
    #[cfg(windows)]
    {
        use usnjmon::windows::converter;
        use usnjmon::windows::event_log;
        use usnjmon::windows::service;
        if args[0] == "verify-resources" && args.len() == 2 {
            event_log::verify_resources(Path::new(&args[1]))?;
            println!(
                "All event messages render correctly; no registration or event writes performed."
            );
            return Ok(());
        }
        if args.len() == 3 {
            if args[0] == "convert" {
                let stop = service::console_stop()?;
                let converted =
                    converter::convert(Path::new(&args[1]), Path::new(&args[2]), &stop)?;
                println!(
                    "Converted {} records; skipped {} zero-padding bytes.\nInput SHA-256: {}\nEVTX: {}",
                    converted.records,
                    converted.padding_bytes,
                    converted.input_sha256,
                    Path::new(&args[2]).display()
                );
                return Ok(());
            }
            if args[0] == "foreground" {
                return service::foreground((&args[1]).into(), (&args[2]).into());
            }
            if args[0] == "service" {
                return service::dispatch((&args[1]).into(), (&args[2]).into());
            }
        }
    }
    bail!("invalid command or unsupported platform; use --help (collector requires Windows x64)")
}
