use anyhow::Result;
use clap::{Parser, Subcommand};
mod init;
mod document;
mod generate;
mod database;
mod embed;
mod net;
mod secrets;
mod utils;

/// Main CLI application
#[derive(Parser)]
#[command(name = "main")]
#[command(about = "Example Rust CLI with subcommands and flags")]
struct Cli {
    #[command(flatten)]
    globals: net::EgressArgs,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Init,
    /// Inspect configuration.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    Db {
        #[arg(long)]
        create: bool,

        #[arg(long)]
        drop: bool,

        #[arg(long)]
        list: bool,

        #[arg(long)]
        table_name: Option<String>,
    },
    Document {
        #[arg(long, value_name = "PATH")]
        path: String,

        #[arg(long)]
        table_name: String,
    },
    Generate {
        #[arg(long)]
        query: Option<String>,

        #[arg(long)]
        table_name: String,

        #[arg(long, default_value_t = 5)]
        top_k: usize,
    }
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Flag plaintext secrets and lax file permissions; exits non-zero on findings.
    Lint,
}

fn main() {
    if let Err(e) = run() {
        // Scrub any registered secret from the error chain before it is printed,
        // so a leaked password can never reach stderr (PRD §6.2 E1.3).
        eprintln!("Error: {}", secrets::redact(&format!("{e:#}")));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let globals = &cli.globals;

    match cli.command {
        Commands::Init => {
            init::init()?;
        },
        Commands::Config { action } => match action {
            ConfigAction::Lint => {
                if utils::lint()? {
                    std::process::exit(1);
                }
            }
        },
        Commands::Db { create, drop, list, table_name } => {
            if !create && !drop && !list {
                eprintln!("No flag provided. Use --help for options.");
                return Ok(());
            }
            let config = utils::load_config(globals)?;
            database::database(create, drop, list, table_name, &config)?;
        },
        Commands::Document { path, table_name } => {
            let config = utils::load_config(globals)?;
            document::document(&path, &table_name, &config)?;
        },
        Commands::Generate { query, table_name, top_k } => {
            let config = utils::load_config(globals)?;
            generate::generate(query.as_deref(), &table_name, top_k, &config)?;
        }
    }
    Ok(())
}
