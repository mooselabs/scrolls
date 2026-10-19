use clap::Parser;
fn main() {
    let cli = match scrolls::cli::Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            if std::env::args().any(|a| a == "--json")
                && !matches!(
                    e.kind(),
                    clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
                )
            {
                println!(
                    "{}",
                    serde_json::json!({"schema_version":2,"ok":false,"error":{"message":e.to_string()}})
                );
                std::process::exit(2)
            }
            e.exit()
        }
    };
    match scrolls::app::run(&cli) {
        Ok(mut v) => {
            let lint_failed = matches!(cli.command, scrolls::cli::Cmd::Lint)
                && v["data"]["errors"]
                    .as_array()
                    .is_some_and(|e| !e.is_empty());
            if lint_failed {
                v["ok"] = serde_json::json!(false);
                v["error"] =
                    serde_json::json!({"message":"structural lint errors; inspect data.errors"});
            }
            if let scrolls::cli::Cmd::Schema {
                command: scrolls::cli::SchemaCommand::Export,
            } = cli.command
            {
                print!("{}", v["data"]["yaml"].as_str().unwrap());
            } else if cli.json {
                println!("{}", serde_json::to_string(&v).unwrap());
            } else {
                print!("{}", scrolls::presentation::human(&v));
            }
            if lint_failed || v["ok"] == false {
                std::process::exit(1)
            }
        }
        Err(e) => {
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({"schema_version":2,"ok":false,"error":{"message":format!("{e:#}"),"diagnostic":if matches!(cli.command, scrolls::cli::Cmd::Backup { .. }) { scrolls::storage::backup::diagnostic(&e) } else { scrolls::source_import::diagnostic(&e) }}})
                );
            } else {
                eprintln!("scrolls: {e:#}");
            }
            std::process::exit(1);
        }
    }
}
