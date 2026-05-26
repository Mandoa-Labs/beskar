use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
mod audit;
mod init;
mod document;
mod fips;
mod generate;
mod database;
mod embed;
mod identity;
mod login;
mod net;
mod observability;
mod policy;
mod redact;
mod scim;
mod secrets;
mod serve;
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
    /// Write config.yaml. Prompts interactively, or runs unattended when every
    /// value is supplied via flags/env (PRD §6.2 E1.10).
    Init(init::InitArgs),
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

        /// Check a corpus's structural integrity (row counts, vector index,
        /// dimension consistency) and exit non-zero on failure (PRD §6.2 E1.12).
        #[arg(long)]
        verify: bool,

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

        /// Direct mode: corpus table prefix in the local Postgres (needs config.yaml).
        #[arg(long)]
        table_name: Option<String>,

        /// Client mode: logical corpus on a `beskar serve` instance. Uses the
        /// short-lived token from `beskar login`; no DB credentials required.
        #[arg(long)]
        corpus: Option<String>,

        /// Client mode: server base URL (overrides the one stored by `beskar login`).
        #[arg(long)]
        server: Option<String>,

        #[arg(long, default_value_t = 5)]
        top_k: usize,
    },
    /// Authenticate to a `beskar serve` instance via SSO and store a short-lived token (PRD §6.3 E2.2).
    Login(login::LoginArgs),
    /// Serve ingest + query over an authenticated HTTP API, reusing the CLI core (PRD §6.3 E2.1).
    Serve(serve::ServeArgs),
    /// Print the version and whether FIPS-validated crypto is active (PRD §6.2 E1.9).
    Version,
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

    // `version` reports the FIPS state (among other things) and must work on any
    // build, even one whose FIPS module fails to load, so handle it first.
    if matches!(cli.command, Commands::Version) {
        println!("beskar {}", env!("CARGO_PKG_VERSION"));
        println!("{}", fips::status_line());
        return Ok(());
    }

    // Every other command performs validated crypto (TLS and/or hashing). On a
    // FIPS build, refuse to run if the validated module can't be activated —
    // fail closed rather than silently use non-validated crypto (PRD §6.2 E1.9).
    fips::activate().context("FIPS mode could not be enabled")?;

    let globals = &cli.globals;
    // Audit sink is resolved from the environment so it is available even for
    // `init`, which runs before any config file exists (PRD §6.2 E1.8).
    let audit = audit::Logger::from_env();

    match cli.command {
        Commands::Version => unreachable!("handled before FIPS activation"),
        Commands::Init(args) => {
            let result = init::init(&args);
            audit.record_result("init", None, &result);
            result?;
        },
        Commands::Config { action } => match action {
            ConfigAction::Lint => {
                let result = utils::lint();
                audit.record_result("config-lint", None, &result);
                if result? {
                    std::process::exit(1);
                }
            }
        },
        Commands::Db { create, drop, list, verify, table_name } => {
            if !create && !drop && !list && !verify {
                eprintln!("No flag provided. Use --help for options.");
                return Ok(());
            }
            let result = (|| {
                let config = utils::load_config(globals)?;
                database::database(create, drop, list, verify, table_name.clone(), &config)
            })();
            audit.record_result("db", table_name.as_deref(), &result);
            result?;
        },
        Commands::Document { path, table_name } => {
            let result = (|| {
                let config = utils::load_config(globals)?;
                document::document(&path, &table_name, &config)
            })();
            audit.record_result("document", Some(table_name.as_str()), &result);
            result?;
        },
        Commands::Generate { query, table_name, corpus, server, top_k } => {
            // Client mode (`--corpus`/`--server`) talks to a `beskar serve`
            // instance with the stored session token and needs no DB creds;
            // direct mode (`--table-name`) loads config.yaml and hits Postgres.
            let target = corpus.clone().or_else(|| table_name.clone());
            let result = (|| {
                if corpus.is_some() || server.is_some() {
                    let corpus = corpus
                        .as_deref()
                        .context("client mode requires --corpus (the logical corpus on the server)")?;
                    login::client_generate(globals, server.as_deref(), corpus, query.as_deref(), top_k)
                } else {
                    let table_name = table_name
                        .as_deref()
                        .context("provide --table-name (direct) or --corpus + login (server)")?;
                    let config = utils::load_config(globals)?;
                    generate::generate(query.as_deref(), table_name, top_k, &config)
                }
            })();
            audit.record_result("generate", target.as_deref(), &result);
            result?;
        }
        Commands::Login(args) => {
            let result = login::login(globals, &args);
            audit.record_result("login", Some(args.server.as_str()), &result);
            result?;
        }
        Commands::Serve(args) => {
            let result = (|| {
                let config = utils::load_config(globals)?;
                serve::serve(&args, &config)
            })();
            audit.record_result("serve", None, &result);
            result?;
        }
    }
    Ok(())
}
