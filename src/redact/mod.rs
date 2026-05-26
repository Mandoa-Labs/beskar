//! Pre-embedding PII/secret redaction hooks (PRD §6.2 E1.11).
//!
//! When enabled in `config.yaml`, [`Redactor`] scrubs configured patterns out of
//! text *before* it is embedded, stored, or sent to a generation provider — so
//! PII or secrets in source documents and queries never leave the machine. See
//! `docs/data-flow.md` for the per-provider "what leaves the machine" statement
//! this control backs.
//!
//! This is distinct from [`crate::secrets::redact`], which scrubs known secret
//! *values* (resolved API keys, passwords) from logs and error output. Here we
//! match *patterns* — built-in detectors and user-supplied regexes — in corpus
//! content and queries. Redaction is a best-effort control, not a guarantee:
//! the built-in detectors favor catching common PII over avoiding the
//! occasional false positive (e.g. `ipv4` also matches a dotted version number).

use anyhow::{bail, Context, Result};
use regex::{NoExpand, Regex};
use serde::Deserialize;

/// The `redaction:` block in `config.yaml`. Disabled unless `enabled: true`, so
/// the default build behaves exactly as before this feature existed.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct RedactionConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Built-in detectors to turn on, by name (see [`PRESETS`]).
    #[serde(default)]
    pub presets: Vec<String>,
    /// Custom user-defined regex rules.
    #[serde(default)]
    pub patterns: Vec<PatternConfig>,
    /// Replacement template for matches without their own. `{name}` expands to
    /// the matching rule's name. Defaults to `[REDACTED:{name}]`.
    #[serde(default)]
    pub replacement: Option<String>,
}

/// A single custom redaction rule from `redaction.patterns`.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct PatternConfig {
    pub name: String,
    pub regex: String,
    #[serde(default)]
    pub replacement: Option<String>,
}

/// Built-in detectors, by preset name. Patterns intentionally use only features
/// the `regex` crate supports (no look-around) and anchor on word boundaries so
/// they match whole tokens.
const PRESETS: &[(&str, &str)] = &[
    ("email", r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}"),
    ("us-ssn", r"\b\d{3}-\d{2}-\d{4}\b"),
    // 13–16 digit runs, optionally split by single spaces/hyphens.
    ("credit-card", r"\b\d(?:[ -]?\d){12,15}\b"),
    ("ipv4", r"\b(?:\d{1,3}\.){3}\d{1,3}\b"),
    ("phone-na", r"\b(?:\+?1[ .-]?)?\(?\d{3}\)?[ .-]?\d{3}[ .-]?\d{4}\b"),
];

const DEFAULT_REPLACEMENT: &str = "[REDACTED:{name}]";

#[derive(Debug)]
struct Rule {
    name: String,
    re: Regex,
    /// Literal replacement text (already expanded; applied with [`NoExpand`] so
    /// a `$` in a user's template is never treated as a capture reference).
    replacement: String,
}

/// A compiled set of redaction rules. Build with [`Redactor::from_config`];
/// `None` means redaction is disabled.
#[derive(Debug)]
pub struct Redactor {
    rules: Vec<Rule>,
}

impl Redactor {
    /// Build a redactor from config. Returns `Ok(None)` when redaction is
    /// disabled, and an error when it is enabled but misconfigured (unknown
    /// preset, invalid regex, or no rules) so the operator fails closed rather
    /// than silently embedding unredacted text.
    pub fn from_config(cfg: &RedactionConfig) -> Result<Option<Self>> {
        if !cfg.enabled {
            return Ok(None);
        }
        let default_tmpl = cfg.replacement.as_deref().unwrap_or(DEFAULT_REPLACEMENT);
        let mut rules = Vec::new();

        for preset in &cfg.presets {
            let pattern = preset_pattern(preset).with_context(|| {
                format!(
                    "unknown redaction preset '{preset}'; known presets: {}",
                    preset_names().join(", ")
                )
            })?;
            rules.push(compile_rule(preset, pattern, default_tmpl)?);
        }
        for p in &cfg.patterns {
            let tmpl = p.replacement.as_deref().unwrap_or(default_tmpl);
            rules.push(compile_rule(&p.name, &p.regex, tmpl)?);
        }

        if rules.is_empty() {
            bail!(
                "redaction is enabled but no presets or patterns are configured; \
                 add `redaction.presets` and/or `redaction.patterns`, or set \
                 `redaction.enabled: false`"
            );
        }
        Ok(Some(Self { rules }))
    }

    /// Redact `text`, returning the scrubbed string and the number of matches
    /// replaced across all rules.
    pub fn redact_counted(&self, text: &str) -> (String, usize) {
        let mut out = text.to_string();
        let mut total = 0usize;
        for rule in &self.rules {
            total += rule.re.find_iter(&out).count();
            out = rule
                .re
                .replace_all(&out, NoExpand(rule.replacement.as_str()))
                .into_owned();
        }
        (out, total)
    }

    /// Redact `text`, discarding the match count.
    pub fn redact(&self, text: &str) -> String {
        self.redact_counted(text).0
    }

    /// Names of the active rules, for the `--verbose` data-flow summary.
    pub fn rule_names(&self) -> Vec<&str> {
        self.rules.iter().map(|r| r.name.as_str()).collect()
    }
}

fn compile_rule(name: &str, pattern: &str, replacement_tmpl: &str) -> Result<Rule> {
    let re = Regex::new(pattern)
        .with_context(|| format!("invalid redaction regex for rule '{name}': {pattern}"))?;
    Ok(Rule {
        name: name.to_string(),
        re,
        replacement: replacement_tmpl.replace("{name}", name),
    })
}

fn preset_pattern(name: &str) -> Option<&'static str> {
    PRESETS.iter().find(|(n, _)| *n == name).map(|(_, p)| *p)
}

fn preset_names() -> Vec<&'static str> {
    PRESETS.iter().map(|(n, _)| *n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled(presets: &[&str], patterns: Vec<PatternConfig>) -> RedactionConfig {
        RedactionConfig {
            enabled: true,
            presets: presets.iter().map(|s| s.to_string()).collect(),
            patterns,
            replacement: None,
        }
    }

    #[test]
    fn disabled_config_yields_no_redactor() {
        let r = Redactor::from_config(&RedactionConfig::default()).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn enabled_with_no_rules_is_an_error() {
        let err = Redactor::from_config(&enabled(&[], vec![])).unwrap_err();
        assert!(err.to_string().contains("no presets or patterns"));
    }

    #[test]
    fn unknown_preset_is_an_error() {
        let err = Redactor::from_config(&enabled(&["not-a-preset"], vec![])).unwrap_err();
        assert!(err.to_string().contains("unknown redaction preset"));
    }

    #[test]
    fn invalid_custom_regex_is_an_error() {
        let bad = PatternConfig {
            name: "broken".into(),
            regex: "(".into(),
            replacement: None,
        };
        let err = Redactor::from_config(&enabled(&[], vec![bad])).unwrap_err();
        assert!(err.to_string().contains("invalid redaction regex"));
    }

    #[test]
    fn email_preset_redacts_addresses() {
        let r = Redactor::from_config(&enabled(&["email"], vec![]))
            .unwrap()
            .unwrap();
        let (out, n) = r.redact_counted("contact jane.doe@example.com today");
        assert_eq!(out, "contact [REDACTED:email] today");
        assert_eq!(n, 1);
    }

    #[test]
    fn ssn_and_credit_card_presets_match() {
        let r = Redactor::from_config(&enabled(&["us-ssn", "credit-card"], vec![]))
            .unwrap()
            .unwrap();
        let out = r.redact("ssn 123-45-6789 card 4111 1111 1111 1111 end");
        assert!(!out.contains("123-45-6789"));
        assert!(!out.contains("4111 1111 1111 1111"));
        assert!(out.contains("[REDACTED:us-ssn]"));
        assert!(out.contains("[REDACTED:credit-card]"));
    }

    #[test]
    fn clean_text_is_unchanged_and_counts_zero() {
        let r = Redactor::from_config(&enabled(&["email", "us-ssn"], vec![]))
            .unwrap()
            .unwrap();
        let (out, n) = r.redact_counted("nothing sensitive here");
        assert_eq!(out, "nothing sensitive here");
        assert_eq!(n, 0);
    }

    #[test]
    fn custom_pattern_uses_its_own_replacement() {
        let pat = PatternConfig {
            name: "employee-id".into(),
            regex: r"EMP-\d{6}".into(),
            replacement: Some("<emp>".into()),
        };
        let r = Redactor::from_config(&enabled(&[], vec![pat]))
            .unwrap()
            .unwrap();
        assert_eq!(r.redact("hi EMP-123456 bye"), "hi <emp> bye");
    }

    #[test]
    fn dollar_signs_in_replacement_are_literal_not_capture_refs() {
        // With NoExpand, a `$1` in the template must survive verbatim rather than
        // being interpreted as a capture-group reference.
        let pat = PatternConfig {
            name: "money".into(),
            regex: r"\bsecret\b".into(),
            replacement: Some("$1-LITERAL".into()),
        };
        let r = Redactor::from_config(&enabled(&[], vec![pat]))
            .unwrap()
            .unwrap();
        assert_eq!(r.redact("a secret b"), "a $1-LITERAL b");
    }

    #[test]
    fn rule_names_lists_presets_then_custom() {
        let pat = PatternConfig {
            name: "custom".into(),
            regex: "x".into(),
            replacement: None,
        };
        let r = Redactor::from_config(&enabled(&["email"], vec![pat]))
            .unwrap()
            .unwrap();
        assert_eq!(r.rule_names(), vec!["email", "custom"]);
    }
}
