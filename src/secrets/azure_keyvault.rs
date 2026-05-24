//! Azure Key Vault secret backend (PRD §6.2 E1.1).
//!
//! Reference form: `azure-keyvault://<vault>/<secret-name>`, where `<vault>` is
//! either a short vault name (resolved to `<vault>.vault.azure.net`) or a full
//! host. The secret is fetched over the Key Vault REST API; all traffic goes
//! through the shared [`HttpClient`], so it is subject to the egress allowlist
//! and `--offline`.
//!
//! ## Authentication (first match wins)
//! 1. `AZURE_KEYVAULT_TOKEN` — a pre-acquired bearer token (handy for CI/tests).
//! 2. Client credentials: `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`,
//!    `AZURE_CLIENT_SECRET` — exchanged for a token at AAD.

use anyhow::{bail, Context, Result};

use super::{SecretBackend, SecretRef};
use crate::net::HttpClient;

const KEYVAULT_API_VERSION: &str = "7.4";
const KEYVAULT_SCOPE: &str = "https://vault.azure.net/.default";

pub struct AzureKeyVault;

impl SecretBackend for AzureKeyVault {
    fn resolve(&self, reference: &SecretRef, http: &HttpClient) -> Result<String> {
        let (vault, secret_name) = reference.location.split_once('/').context(
            "azure-keyvault reference must be 'azure-keyvault://<vault>/<secret-name>'",
        )?;
        if secret_name.is_empty() {
            bail!("azure-keyvault reference is missing a secret name");
        }

        let host = if vault.contains('.') {
            vault.to_string()
        } else {
            format!("{vault}.vault.azure.net")
        };
        let token = acquire_token(http)?;

        let url = format!(
            "https://{host}/secrets/{secret_name}?api-version={KEYVAULT_API_VERSION}"
        );
        let resp = http
            .get(&url)?
            .bearer_auth(&token)
            .send()
            .context("failed to call Azure Key Vault")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            bail!("Azure Key Vault returned {status} for secret '{secret_name}': {body}");
        }

        let json: serde_json::Value =
            resp.json().context("failed to parse Key Vault response")?;
        json["value"]
            .as_str()
            .map(str::to_string)
            .context("Key Vault response did not contain a 'value' field")
    }
}

/// Acquire an AAD bearer token for the Key Vault scope.
fn acquire_token(http: &HttpClient) -> Result<String> {
    if let Ok(token) = std::env::var("AZURE_KEYVAULT_TOKEN") {
        if !token.trim().is_empty() {
            return Ok(token);
        }
    }

    let tenant = std::env::var("AZURE_TENANT_ID").ok();
    let client_id = std::env::var("AZURE_CLIENT_ID").ok();
    let client_secret = std::env::var("AZURE_CLIENT_SECRET").ok();

    match (tenant, client_id, client_secret) {
        (Some(tenant), Some(client_id), Some(client_secret)) => {
            client_credentials_token(http, &tenant, &client_id, &client_secret)
        }
        _ => bail!(
            "no Azure credentials found for Key Vault. Set AZURE_KEYVAULT_TOKEN, \
             or AZURE_TENANT_ID + AZURE_CLIENT_ID + AZURE_CLIENT_SECRET"
        ),
    }
}

fn client_credentials_token(
    http: &HttpClient,
    tenant: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<String> {
    let url = format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token");
    let params = [
        ("grant_type", "client_credentials"),
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("scope", KEYVAULT_SCOPE),
    ];
    let resp = http
        .post(&url)?
        .form(&params)
        .send()
        .context("failed to acquire Azure AD token")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        bail!("Azure AD token endpoint returned {status}: {body}");
    }

    let json: serde_json::Value =
        resp.json().context("failed to parse Azure AD token response")?;
    json["access_token"]
        .as_str()
        .map(str::to_string)
        .context("Azure AD token response did not contain 'access_token'")
}
