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
fn build_yaml(pat: &str, connection_string: &str) -> String {
    format!(
        r#"
           pat: {}
           connection_string : {}
        "#,
        pat, connection_string
    )
}

pub fn init(){
    let pat = user_input("Enter PAT: ");
    let connect_string:String = user_input("Enter the connection string: ");

    let yaml = build_yaml(&pat, &connect_string);
    write(&yaml).expect("Failed to write to file");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_build_yaml() {
        let yaml = build_yaml("test_pat", "host=localhost dbname=test");
        assert!(yaml.contains("pat: test_pat"));
        assert!(yaml.contains("connection_string : host=localhost dbname=test"));
    }

    #[test]
    fn test_write_and_read_config() {
        let yaml = build_yaml("my_pat", "host=localhost dbname=mydb");
        write(&yaml).expect("Failed to write config");

        let dir = config_dir();
        let path = format!("{}/config.yaml", dir);
        let contents = fs::read_to_string(&path).expect("Failed to read config file");
        assert!(contents.contains("pat: my_pat"));
        assert!(contents.contains("connection_string : host=localhost dbname=mydb"));

        // Verify file permissions are 0600
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::metadata(&path).expect("Failed to get metadata");
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

