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
fn build_yaml(pat: &str, pghost: &str, pguser: &str, pgport: &str, pgdatabase: &str, pgpassword: &str) -> String {
    format!(
        r#"pat: {}
pghost: {}
pguser: {}
pgport: {}
pgdatabase: {}
pgpassword: {}
"#,
        pat, pghost, pguser, pgport, pgdatabase, pgpassword
    )
}

pub fn init(){
    let pat = user_input("Enter PAT: ");
    let pghost = user_input("Enter PGHOST: ");
    let pguser = user_input("Enter PGUSER: ");
    let pgport = user_input("Enter PGPORT (default 5432): ");
    let pgport = if pgport.is_empty() { "5432".to_string() } else { pgport };
    let pgdatabase = user_input("Enter PGDATABASE (default postgres): ");
    let pgdatabase = if pgdatabase.is_empty() { "postgres".to_string() } else { pgdatabase };
    let pgpassword = user_input("Enter PGPASSWORD: ");

    let yaml = build_yaml(&pat, &pghost, &pguser, &pgport, &pgdatabase, &pgpassword);
    write(&yaml).expect("Failed to write to file");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_build_yaml() {
        let yaml = build_yaml("test_pat", "localhost", "user", "5432", "testdb", "pass");
        assert!(yaml.contains("pat: test_pat"));
        assert!(yaml.contains("pghost: localhost"));
        assert!(yaml.contains("pguser: user"));
        assert!(yaml.contains("pgport: 5432"));
        assert!(yaml.contains("pgdatabase: testdb"));
        assert!(yaml.contains("pgpassword: pass"));
    }

    #[test]
    fn test_write_and_read_config() {
        let yaml = build_yaml("my_pat", "myhost", "myuser", "5432", "mydb", "mypass");
        write(&yaml).expect("Failed to write config");

        let dir = config_dir();
        let path = format!("{}/config.yaml", dir);
        let contents = fs::read_to_string(&path).expect("Failed to read config file");
        assert!(contents.contains("pat: my_pat"));
        assert!(contents.contains("pghost: myhost"));

        // Verify file permissions are 0600
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::metadata(&path).expect("Failed to get metadata");
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

