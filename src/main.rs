// use std::io;
use clap::{Parser, Subcommand};
mod init;
mod document;
mod generate;
mod database;

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
    Init,
    Db {
        #[arg(long)]
        create: bool,

        #[arg(long)]
        drop: bool,

        #[arg(long)]
        list: bool,
    },
    Document {
        #[arg(long, value_name = "PATH")]
        path: String,
    },
    Generate
} 

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init => {
            init::init();
        },
        Commands::Db { create, drop, list } => {
            if !create && !drop  && !list {
                eprintln!("No flag provided. Use --help for options.");
                return;
            }
            database::database( create, drop, list);
        },
        Commands::Document { path } => {
            document::document(&path);
        },
        Commands::Generate => {
            generate::generate();
        }
    }
}
