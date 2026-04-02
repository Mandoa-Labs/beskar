use std::fs;
use std::io;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
// use crate::utils;

fn config_dir() -> String {
    let home = dirs::config_dir().expect("Could not determine config directory");
    format!("{}/beskar", home.display())
}

fn write(yaml: &str) -> io::Result<()> {
    let dir = config_dir();
    fs::create_dir_all(&dir)?;

    let state_file = format!("{}/config.yaml", dir);
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&state_file)?
        .write_all(yaml.as_bytes())?;

    Ok(())
}

fn user_input(prompt: &str) -> String {
    use std::io::{stdin,stdout};
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

    // let contents = utils::read_yaml("/var/lib/beskar/config.yaml").expect("Failed to read from file");
    // println!("File contents: {}", contents);

}

