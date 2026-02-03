use std::fs;
use std::io;

fn write() -> io::Result<()> {
    let dir = "/var/lib/beskar";
    fs::create_dir_all(dir)?;

    let state_file = format!("{}/state.json", dir);
    fs::write(&state_file, r#"{"counter": 42}"#)?;

    Ok(())
}

fn read() -> io::Result<String> {
    let dir = "/var/lib/beskar";
    let state_file = format!("{}/state.json", dir);
    let contents = fs::read_to_string(&state_file)?;
    Ok(contents)
}

pub fn init(){
    hello();
    write().expect("Failed to write to file");
    let contents = read().expect("Failed to read from file");
    println!("File contents: {}", contents);
    println!("Hello from init!")
}

fn hello() {
    println!("Hello, from hello func!");
}