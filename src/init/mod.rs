use std::fs;
use std::io;

fn write(yaml: &str) -> io::Result<()> {
    let dir = "/var/lib/beskar";
    fs::create_dir_all(dir)?;

    let state_file = format!("{}/config.yaml", dir);
    fs::write(&state_file, yaml)?;

    Ok(())
}

fn read() -> io::Result<String> {
    let dir = "/var/lib/beskar";
    let state_file = format!("{}/config.yaml", dir);
    let contents = fs::read_to_string(&state_file)?;
    Ok(contents)
}

pub fn init(){

    let mut yaml = String::from("name: beskar\nversion: 0.1.0\n");

    yaml += "settings:\n  option1: true\n  option2: false\n";

    write(&yaml).expect("Failed to write to file");

    let contents = read().expect("Failed to read from file");
    println!("File contents: {}", contents);
    println!("Hello from init!")
}

