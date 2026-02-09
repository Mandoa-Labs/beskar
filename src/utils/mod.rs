use std::fs;
use std::io;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Config {
    pat: String,
    connection_string: String,
}

fn read_yaml_file(path: &str) -> io::Result<Config> {
    let contents = fs::read_to_string(path)?;
    let config: Config = serde_yaml::from_str(&contents)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(config)
}

pub fn read_yaml(path: &str) -> io::Result<()> {
    let config = read_yaml_file(path)?;

    println!("PAT: {}", config.pat);
    println!("Connection String: {}", config.connection_string);

    Ok(())
}
