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

fn append(content: &str) -> io::Result<()> {
    let dir = "/var/lib/beskar";
    let state_file = format!("{}/config.yaml", dir);
    let mut existing_content = fs::read_to_string(&state_file)?;
    existing_content.push('\n');
    existing_content.push_str(content);
    fs::write(&state_file, existing_content)?;
    Ok(())
}

pub fn init(){
    write("person: ").expect("Failed to write to file");
    append("    name: John Smith").expect("Failed to write to file");
    append("    age: 33").expect("Failed to write to file");
    append("    gender: Male").expect("Failed to write to file");
    append("    is_student: false").expect("Failed to write to file");
    append("    address: ").expect("Failed to write to file");
    append("        street: 123 Main Street").expect("Failed to write to file");
    append("        city: Anywhere").expect("Failed to write to file");
    append("        state: CA").expect("Failed to write to file");
    append("        zipcode: \"90210\"").expect("Failed to write to file");
    append("\n").expect("Failed to write to file");

    let contents = read().expect("Failed to read from file");
    println!("File contents: {}", contents);
    println!("Hello from init!")
}

