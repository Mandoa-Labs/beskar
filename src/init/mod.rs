use std::fs::File;
use std::io::Write;
use std::io;

fn write() -> io::Result<()> {
    let mut file = File::create("example.txt")?;
    file.write_all(b"Hello, world!\n")?;
    Ok(())
}

pub fn init(){
    hello();
    write().expect("Failed to write to file");
    println!("Hello from init!")
}

fn hello() {
    println!("Hello, from hello func!");
}