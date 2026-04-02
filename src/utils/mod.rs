use std::fs;
use std::io;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub pat: String,
    pub connection_string: String,
}

fn read_yaml_file(path: &str) -> io::Result<Config> {
    let contents = fs::read_to_string(path)?;
    let config: Config = serde_yaml::from_str(&contents)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(config)
}

pub fn read_config() -> io::Result<Config> {
    let dir = dirs::config_dir().expect("Could not determine config directory");
    let path = format!("{}/beskar/config.yaml", dir.display());
    read_yaml_file(&path)
}

pub fn read_yaml(path: &str) -> io::Result<()> {
    let config = read_yaml_file(path)?;

    println!("PAT: {}", config.pat);
    println!("Connection String: {}", config.connection_string);

    Ok(())
}
