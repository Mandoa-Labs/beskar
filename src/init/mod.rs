use std::fs;
use std::io;
use std::io::Write;
use anyhow::{bail, Context, Result};

/// Flags for `beskar init`. Every interactive prompt has a flag and an
/// environment-variable equivalent so Beskar runs unattended in CI and
/// golden-image builds (PRD §6.2 E1.10). Resolution order per field is
/// flag → env → interactive prompt; with `--non-interactive`, a missing
/// required value is a hard error instead of a prompt.
#[derive(clap::Args, Debug, Default)]
pub struct InitArgs {
    /// OpenAI API key used for embeddings (env: BESKAR_PAT, OPENAI_API_KEY).
    /// Not required when embeddings and generation both use Ollama.
    #[arg(long)]
    pub pat: Option<String>,
    /// `generate` provider: openai | anthropic | ollama (env: BESKAR_PROVIDER; default openai).
    #[arg(long)]
    pub provider: Option<String>,
    /// Anthropic API key, required only when provider=anthropic (env: BESKAR_ANTHROPIC_KEY, ANTHROPIC_API_KEY).
    #[arg(long)]
    pub anthropic_key: Option<String>,
    /// `embed` provider: openai | ollama (env: BESKAR_EMBED_PROVIDER; default openai).
    #[arg(long)]
    pub embed_provider: Option<String>,
    /// Ollama host base URL, used when embed/generate provider is ollama
    /// (env: BESKAR_OLLAMA_HOST, OLLAMA_HOST; default http://127.0.0.1:11434).
    #[arg(long)]
    pub ollama_host: Option<String>,
    /// Embedding model override (env: BESKAR_EMBED_MODEL; e.g. nomic-embed-text for ollama).
    #[arg(long)]
    pub embed_model: Option<String>,
    /// Generation model override (env: BESKAR_GENERATE_MODEL; e.g. llama3.1 for ollama).
    #[arg(long)]
    pub generate_model: Option<String>,
    /// Postgres host (env: PGHOST).
    #[arg(long)]
    pub pghost: Option<String>,
    /// Postgres user (env: PGUSER).
    #[arg(long)]
    pub pguser: Option<String>,
    /// Postgres port (env: PGPORT; default 5432).
    #[arg(long)]
    pub pgport: Option<String>,
    /// Postgres database (env: PGDATABASE; default postgres).
    #[arg(long)]
    pub pgdatabase: Option<String>,
    /// Postgres password (env: PGPASSWORD).
    #[arg(long)]
    pub pgpassword: Option<String>,
    /// Never prompt: fail if any required value is missing. For CI / golden images.
    #[arg(long)]
    pub non_interactive: bool,
}

fn config_dir() -> String {
    let home = dirs::config_dir().expect("Could not determine config directory");
    format!("{}/beskar", home.display())
}

#[cfg(unix)]
fn write(yaml: &str) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

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

#[cfg(windows)]
fn write(yaml: &str) -> io::Result<()> {
    let dir = config_dir();
    fs::create_dir_all(&dir)?;

    let state_file = format!("{}/config.yaml", dir);
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&state_file)?
        .write_all(yaml.as_bytes())?;

    eprintln!(
        "warning: wrote {} with default Windows ACLs; restrict access manually if needed.",
        state_file
    );

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
/// Everything `beskar init` resolves before writing `config.yaml`.
#[derive(Default)]
struct ConfigInput {
    pat: String,
    /// `generate` provider (top-level `provider:`).
    provider: String,
    anthropic_key: String,
    /// `embed` provider; only written as an `embed:` block when not the default.
    embed_provider: String,
    /// Ollama host; only written when an endpoint uses ollama.
    ollama_host: String,
    embed_model: String,
    generate_model: String,
    pghost: String,
    pguser: String,
    pgport: String,
    pgdatabase: String,
    pgpassword: String,
}

fn build_yaml(c: &ConfigInput) -> String {
    let mut out = String::new();
    // `pat` is optional now (an Ollama-only config needs no OpenAI key); omit the
    // key entirely when empty so it deserializes via serde's default.
    if !c.pat.is_empty() {
        out.push_str(&format!("pat: {}\n", c.pat));
    }
    out.push_str(&format!("provider: {}\n", c.provider));
    if !c.anthropic_key.is_empty() {
        out.push_str(&format!("anthropic_key: {}\n", c.anthropic_key));
    }
    if !c.ollama_host.is_empty() {
        out.push_str(&format!("ollama_host: {}\n", c.ollama_host));
    }
    // Emit an `embed:` block when the embed provider isn't the default, or a
    // model override is given.
    if c.embed_provider != "openai" || !c.embed_model.is_empty() {
        out.push_str("embed:\n");
        out.push_str(&format!("  provider: {}\n", c.embed_provider));
        if !c.embed_model.is_empty() {
            out.push_str(&format!("  model: {}\n", c.embed_model));
        }
    }
    // The generation provider is set top-level; only emit a `generate:` block for
    // a model override.
    if !c.generate_model.is_empty() {
        out.push_str("generate:\n");
        out.push_str(&format!("  model: {}\n", c.generate_model));
    }
    out.push_str(&format!(
        r#"pghost: {}
pguser: {}
pgport: {}
pgdatabase: {}
pgpassword: {}
"#,
        c.pghost, c.pguser, c.pgport, c.pgdatabase, c.pgpassword
    ));
    out
}

/// First non-empty value among the given environment variables.
fn from_env(vars: &[&str]) -> Option<String> {
    vars.iter().find_map(|var| {
        std::env::var(var)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    })
}

/// Resolve a required field: flag → env → prompt. In non-interactive mode a
/// missing value is a hard error naming the env vars that would satisfy it.
fn required(
    flag: &Option<String>,
    env: &[&str],
    label: &str,
    non_interactive: bool,
) -> Result<String> {
    if let Some(v) = flag.clone().filter(|s| !s.is_empty()).or_else(|| from_env(env)) {
        return Ok(v);
    }
    if non_interactive {
        bail!("missing required value for {label}; set one of [{}] or pass the matching flag (--non-interactive)", env.join(", "));
    }
    let v = user_input(&format!("Enter {label}: "));
    if v.is_empty() {
        bail!("{label} is required");
    }
    Ok(v)
}

/// Resolve an optional field with a default: flag → env → prompt, falling back
/// to `default` when nothing is supplied (and immediately, in non-interactive
/// mode).
fn optional(
    flag: &Option<String>,
    env: &[&str],
    label: &str,
    default: &str,
    non_interactive: bool,
) -> String {
    if let Some(v) = flag.clone().filter(|s| !s.is_empty()).or_else(|| from_env(env)) {
        return v;
    }
    if non_interactive {
        return default.to_string();
    }
    let v = user_input(&format!("Enter {label}: "));
    if v.is_empty() { default.to_string() } else { v }
}

pub fn init(args: &InitArgs) -> Result<()> {
    let ni = args.non_interactive;
    let provider = optional(&args.provider, &["BESKAR_PROVIDER"],
        "PROVIDER for `generate` (openai | anthropic | ollama, default openai)", "openai", ni);
    let embed_provider = optional(&args.embed_provider, &["BESKAR_EMBED_PROVIDER"],
        "EMBED provider (openai | ollama, default openai)", "openai", ni);

    // The OpenAI `pat` is only needed when embeddings or generation actually use
    // an OpenAI key. With Ollama on both sides, skip it entirely (OL.1).
    let needs_pat = embed_provider != "ollama" || provider == "openai";
    let pat = if needs_pat {
        required(&args.pat, &["BESKAR_PAT", "OPENAI_API_KEY"],
            "PAT (OpenAI key, used for embeddings)", ni)?
    } else {
        from_env(&["BESKAR_PAT", "OPENAI_API_KEY"])
            .or_else(|| args.pat.clone().filter(|s| !s.is_empty()))
            .unwrap_or_default()
    };

    let anthropic_key = if provider == "anthropic" {
        required(&args.anthropic_key, &["BESKAR_ANTHROPIC_KEY", "ANTHROPIC_API_KEY"],
            "ANTHROPIC_KEY", ni)?
    } else {
        // Accept one if offered, but never require or prompt for it.
        from_env(&["BESKAR_ANTHROPIC_KEY", "ANTHROPIC_API_KEY"])
            .or_else(|| args.anthropic_key.clone().filter(|s| !s.is_empty()))
            .unwrap_or_default()
    };

    // Ollama host + model overrides, only relevant (and prompted for) when an
    // endpoint uses ollama.
    let uses_ollama = embed_provider == "ollama" || provider == "ollama";
    let ollama_host = if uses_ollama {
        optional(&args.ollama_host, &["BESKAR_OLLAMA_HOST", "OLLAMA_HOST"],
            "OLLAMA_HOST (default http://127.0.0.1:11434)", "http://127.0.0.1:11434", ni)
    } else {
        String::new()
    };
    // Model overrides are always optional; never prompt for them (env/flag only).
    let embed_model = args.embed_model.clone().filter(|s| !s.is_empty())
        .or_else(|| from_env(&["BESKAR_EMBED_MODEL"]))
        .unwrap_or_default();
    let generate_model = args.generate_model.clone().filter(|s| !s.is_empty())
        .or_else(|| from_env(&["BESKAR_GENERATE_MODEL"]))
        .unwrap_or_default();

    let pghost = required(&args.pghost, &["PGHOST"], "PGHOST", ni)?;
    let pguser = required(&args.pguser, &["PGUSER"], "PGUSER", ni)?;
    let pgport = optional(&args.pgport, &["PGPORT"], "PGPORT (default 5432)", "5432", ni);
    let pgdatabase = optional(&args.pgdatabase, &["PGDATABASE"],
        "PGDATABASE (default postgres)", "postgres", ni);
    let pgpassword = required(&args.pgpassword, &["PGPASSWORD"], "PGPASSWORD", ni)?;

    let yaml = build_yaml(&ConfigInput {
        pat, provider, anthropic_key, embed_provider, ollama_host,
        embed_model, generate_model, pghost, pguser, pgport, pgdatabase, pgpassword,
    });
    write(&yaml).context("failed to write config")?;
    println!("Wrote config to {}/config.yaml", config_dir());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn optional_uses_default_when_unset_and_non_interactive() {
        // An env var name that is not set, so resolution falls through to default.
        let v = optional(&None, &["BESKAR_TEST_UNSET_OPTIONAL"], "X", "fallback", true);
        assert_eq!(v, "fallback");
    }

    #[test]
    fn required_errors_when_missing_and_non_interactive() {
        let r = required(&None, &["BESKAR_TEST_UNSET_REQUIRED"], "Y", true);
        assert!(r.is_err());
    }

    #[test]
    fn flag_takes_precedence_over_default() {
        let v = optional(
            &Some("flagval".to_string()),
            &["BESKAR_TEST_UNSET_FLAG"],
            "Z",
            "def",
            true,
        );
        assert_eq!(v, "flagval");
        let r = required(&Some("req".to_string()), &["BESKAR_TEST_UNSET_FLAG"], "Z", true).unwrap();
        assert_eq!(r, "req");
    }

    fn base_input() -> ConfigInput {
        ConfigInput {
            pat: "test_pat".into(),
            provider: "openai".into(),
            embed_provider: "openai".into(),
            pghost: "localhost".into(),
            pguser: "user".into(),
            pgport: "5432".into(),
            pgdatabase: "testdb".into(),
            pgpassword: "pass".into(),
            ..Default::default()
        }
    }

    #[test]
    fn test_build_yaml() {
        let yaml = build_yaml(&base_input());
        assert!(yaml.contains("pat: test_pat"));
        assert!(yaml.contains("provider: openai"));
        assert!(!yaml.contains("anthropic_key"));
        // Default openai embed provider with no override emits no embed block.
        assert!(!yaml.contains("embed:"));
        assert!(!yaml.contains("ollama_host"));
        assert!(yaml.contains("pghost: localhost"));
        assert!(yaml.contains("pguser: user"));
        assert!(yaml.contains("pgport: 5432"));
        assert!(yaml.contains("pgdatabase: testdb"));
        assert!(yaml.contains("pgpassword: pass"));
    }

    #[test]
    fn test_build_yaml_with_anthropic_key() {
        let yaml = build_yaml(&ConfigInput {
            provider: "anthropic".into(),
            anthropic_key: "sk-ant-xxx".into(),
            ..base_input()
        });
        assert!(yaml.contains("provider: anthropic"));
        assert!(yaml.contains("anthropic_key: sk-ant-xxx"));
    }

    #[test]
    fn test_build_yaml_ollama_omits_pat_and_writes_blocks() {
        let yaml = build_yaml(&ConfigInput {
            pat: String::new(),
            provider: "ollama".into(),
            embed_provider: "ollama".into(),
            ollama_host: "http://ollama.internal:11434".into(),
            ..base_input()
        });
        // No OpenAI key for an Ollama-only config.
        assert!(!yaml.contains("pat:"));
        assert!(yaml.contains("provider: ollama"));
        assert!(yaml.contains("ollama_host: http://ollama.internal:11434"));
        assert!(yaml.contains("embed:\n  provider: ollama"));
    }

    #[test]
    fn test_build_yaml_writes_model_overrides() {
        let yaml = build_yaml(&ConfigInput {
            embed_provider: "ollama".into(),
            embed_model: "nomic-embed-text".into(),
            generate_model: "llama3.1".into(),
            ..base_input()
        });
        assert!(yaml.contains("embed:\n  provider: ollama\n  model: nomic-embed-text"));
        assert!(yaml.contains("generate:\n  model: llama3.1"));
    }

    #[test]
    fn test_write_and_read_config() {
        let yaml = build_yaml(&ConfigInput {
            pat: "my_pat".into(),
            pghost: "myhost".into(),
            pguser: "myuser".into(),
            pgdatabase: "mydb".into(),
            pgpassword: "mypass".into(),
            ..base_input()
        });
        write(&yaml).expect("Failed to write config");

        let dir = config_dir();
        let path = format!("{}/config.yaml", dir);
        let contents = fs::read_to_string(&path).expect("Failed to read config file");
        assert!(contents.contains("pat: my_pat"));
        assert!(contents.contains("pghost: myhost"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = fs::metadata(&path).expect("Failed to get metadata");
            let mode = metadata.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}
