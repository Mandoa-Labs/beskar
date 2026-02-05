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

fn user_input(prompt: &str) -> String {
    use std::io::{stdin,stdout,Write};
    let mut _s=String::new();
    print!("{}", prompt);
    let _=stdout().flush();
    stdin().read_line(&mut _s).expect("Did not enter a correct string");
    if let Some('\n')=_s.chars().next_back() {
        _s.pop();
    }
    if let Some('\r')=_s.chars().next_back() {
        _s.pop();
    }

    _s
}
pub fn init(){
    let pat = user_input("Enter PAT: ");
    let connect_string:String = user_input("Enter the connection string: ");

    let yaml= format!(
        r#"
           pat: {}
           connection_string : {}   
        "#,
        pat, connect_string
    );

    write(&yaml).expect("Failed to write to file");

    let contents = read().expect("Failed to read from file");
    println!("File contents: {}", contents);

}

