use crate::utils;
use openssl::ssl::{SslConnector, SslMethod};
use postgres::Client;
use postgres_openssl::MakeTlsConnector;

fn connect(config: &utils::Config) -> Client {
    let conn_string = format!(
        "host={} user={} port={} dbname={} password={} sslmode=require",
        config.pghost, config.pguser, config.pgport, config.pgdatabase, config.pgpassword
    );

    let builder = SslConnector::builder(SslMethod::tls()).expect("Failed to create SSL connector");
    let connector = MakeTlsConnector::new(builder.build());

    Client::connect(&conn_string, connector)
        .expect("Failed to connect to PostgreSQL")
}

pub fn database(create: bool, drop: bool, list: bool, table_name: Option<String>) {
    let config = utils::read_config().expect("Failed to read config. Run `beskar init` first.");

    if create {
        let name = table_name.expect("--table-name is required with --create");
        create_table(&config, &name);
    }
    if drop {
        println!("Dropping database...");
    }
    if list {
        println!("Listing databases...");
    }
}

fn create_table(config: &utils::Config, table_name: &str) {
    let mut client = connect(config);
    let query = format!(
        "CREATE TABLE IF NOT EXISTS {} (id SERIAL PRIMARY KEY)",
        table_name
    );
    client.execute(&query[..], &[]).expect("Failed to create table");
    println!("Table '{}' created successfully.", table_name);
}
