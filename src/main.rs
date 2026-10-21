use anyhow::{Result, bail};

fn main() {
    if let Err(error) = main_result() {
        eprintln!("usn-eventlog: {error:#}");
        std::process::exit(1);
    }
}

fn main_result() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.is_empty() || args[0] == "--help" {
        println!(
            "usn-eventlog 0.3.0\n\
            Usage:\n  usn-eventlog foreground <config.json> <state-directory>\n  \
            usn-eventlog service <config.json> <state-directory>\n  \
            usn-eventlog verify-resources <message.dll>\n  usn-eventlog check-config <config.json>\n\
            Foreground and service modes WRITE to UsnEventLog/Operational.\n\
            Install explicitly with scripts\\Install.ps1. No journal is created or modified."
        );
        return Ok(());
    }
    if args[0] == "check-config" && args.len() == 2 {
        usn_eventlog::config::Config::load(std::path::Path::new(&args[1]))?;
        println!("Configuration valid.");
        return Ok(());
    }
    #[cfg(windows)]
    {
        use usn_eventlog::windows::{eventlog, service};
        if args[0] == "verify-resources" && args.len() == 2 {
            eventlog::verify_resources(std::path::Path::new(&args[1]))?;
            println!(
                "All event messages render correctly; no registration or event writes performed."
            );
            return Ok(());
        }
        if args.len() == 3 {
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
