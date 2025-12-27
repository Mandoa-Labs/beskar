
use clap::{Parser, Subcommand};

/// Main CLI application
#[derive(Parser)]
#[command(name = "main")]
#[command(about = "Example Rust CLI with subcommands and flags")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Database related commands
    Db {
        /// Create the database
        #[arg(long)]
        create: bool,

        /// Drop the database
        #[arg(long)]
        drop: bool,

        /// List databases
        #[arg(long)]
        list: bool,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Db { create, drop, list } => {
            if create {
                println!("Creating database...");
                // create_db();
            }

            if drop {
                println!("Dropping database...");
                // drop_db();
            }

            if list {
                println!("Listing databases...");
                // list_dbs();
            }

            if !create && !drop && !list {
                eprintln!("No flag provided. Use --help for options.");
            }
        }
    }
}
