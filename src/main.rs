#![allow(warnings)]
// use std::io;
use clap::{Parser, Subcommand};
mod init;
mod document;
mod generate;
mod database;
mod embed;
mod utils;

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

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init => {
            init::init();
        },
        Commands::Db { create, drop, list, table_name } => {
            if !create && !drop  && !list {
                eprintln!("No flag provided. Use --help for options.");
                return;
            }
            database::database(create, drop, list, table_name);
        },
        Commands::Document { path, table_name } => {
            document::document(&path, &table_name);
        },
        Commands::Generate { query, table_name, top_k } => {
            generate::generate(query.as_deref(), &table_name, top_k);
        }
    }
}
