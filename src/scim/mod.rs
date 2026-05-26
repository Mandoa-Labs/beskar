//! SCIM 2.0 provisioning (PRD §6.3 E2.4 · §15).
//!
//! `beskar serve` mounts the SCIM 2.0 protocol under `/scim/v2/*` so an identity
//! provider (Okta, Microsoft Entra ID, …) can **provision and deprovision** users
//! and groups in Beskar. Creating a user in the IdP `POST`s a User resource here;
//! deactivating one arrives as a `PATCH` (`active=false`) or a `DELETE`. State is
//! stored in the operator's own Postgres (the platform's durable store), so it
//! survives restarts and is the system of record the IdP reconciles against.
//!
//! Authentication reuses the server's bearer token (the IdP's "SCIM bearer
//! token"); routing/auth live in [`crate::serve`], and the request-handling logic
//! here is storage-agnostic via the [`ScimStore`] trait — [`MemoryStore`] backs
//! the unit tests, [`PgStore`] backs the running server.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Context, Result};
use postgres::Row;
use serde_json::{json, Value};

use crate::utils::Config;
use crate::{database, secrets};

// SCIM 2.0 schema URNs (RFC 7643 / 7644).
const SCHEMA_USER: &str = "urn:ietf:params:scim:schemas:core:2.0:User";
const SCHEMA_GROUP: &str = "urn:ietf:params:scim:schemas:core:2.0:Group";
const SCHEMA_LIST: &str = "urn:ietf:params:scim:api:messages:2.0:ListResponse";
const SCHEMA_ERROR: &str = "urn:ietf:params:scim:api:messages:2.0:Error";
const SCHEMA_SPC: &str = "urn:ietf:params:scim:schemas:core:2.0:ServiceProviderConfig";

// ---------------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------------

/// A provisioned SCIM user. Only the attributes IdPs reliably send for
/// provisioning are modeled; unknown attributes are accepted but not persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScimUser {
    pub id: String,
    pub user_name: String,
    pub active: bool,
    pub external_id: Option<String>,
    pub display_name: Option<String>,
    pub given_name: Option<String>,
    pub family_name: Option<String>,
    pub emails: Vec<String>,
    /// RFC 3339 timestamps for `meta`.
    pub created: String,
    pub last_modified: String,
}

impl ScimUser {
    /// Parse the mutable attributes from a SCIM User resource. `id`/`created`/
    /// `last_modified` are assigned by the server, not the client.
    fn from_resource(v: &Value) -> Result<ScimUser, String> {
        let user_name =
            string_field(v, "userName").ok_or_else(|| "userName is required".to_string())?;
        let (given_name, family_name) = match v.get("name") {
            Some(n) => (string_field(n, "givenName"), string_field(n, "familyName")),
            None => (None, None),
        };
        Ok(ScimUser {
            id: String::new(),
            user_name,
            active: v.get("active").map(parse_bool_value).unwrap_or(true),
            external_id: string_field(v, "externalId"),
            display_name: string_field(v, "displayName"),
            given_name,
            family_name,
            emails: parse_emails(v.get("emails")),
            created: String::new(),
            last_modified: String::new(),
        })
    }

    /// Serialize to a SCIM User resource JSON object.
    fn to_resource(&self) -> Value {
        let mut name = serde_json::Map::new();
        if let Some(g) = &self.given_name {
            name.insert("givenName".into(), json!(g));
        }
        if let Some(f) = &self.family_name {
            name.insert("familyName".into(), json!(f));
        }
        match (&self.given_name, &self.family_name) {
            (Some(g), Some(f)) => {
                name.insert("formatted".into(), json!(format!("{g} {f}")));
            }
            (Some(g), None) => {
                name.insert("formatted".into(), json!(g));
            }
            (None, Some(f)) => {
                name.insert("formatted".into(), json!(f));
            }
            (None, None) => {}
        }

        let emails: Vec<Value> = self
            .emails
            .iter()
            .enumerate()
            .map(|(i, e)| json!({"value": e, "primary": i == 0}))
            .collect();

        let mut obj = json!({
            "schemas": [SCHEMA_USER],
            "id": self.id,
            "userName": self.user_name,
            "active": self.active,
            "meta": {
                "resourceType": "User",
                "created": self.created,
                "lastModified": self.last_modified,
                "location": format!("/scim/v2/Users/{}", self.id),
            }
        });
        if let Some(e) = &self.external_id {
            obj["externalId"] = json!(e);
        }
        if let Some(d) = &self.display_name {
            obj["displayName"] = json!(d);
        }
        if !name.is_empty() {
            obj["name"] = Value::Object(name);
        }
        if !emails.is_empty() {
            obj["emails"] = json!(emails);
        }
        obj
    }

    /// Apply a SCIM PATCH (`urn:...:PatchOp`) in place. Supports the operations
    /// IdPs use for provisioning — most importantly `replace active=false` to
    /// deactivate — both with an attribute `path` and as a pathless object merge
    /// (Microsoft Entra ID). Unknown paths are ignored, per the lenient-server
    /// guidance in RFC 7644 §3.5.2.
    fn apply_patch(&mut self, patch: &Value) -> Result<(), String> {
        let ops = patch
            .get("Operations")
            .or_else(|| patch.get("operations"))
            .and_then(Value::as_array)
            .ok_or_else(|| "PATCH body requires an Operations array".to_string())?;
        for op in ops {
            let name = op
                .get("op")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            let path = op.get("path").and_then(Value::as_str).map(str::trim);
            let value = op.get("value");
            match name.as_str() {
                "replace" | "add" => self.set(path, value)?,
                "remove" => self.remove(path),
                other => return Err(format!("unsupported PATCH op '{other}'")),
            }
        }
        Ok(())
    }

    fn set(&mut self, path: Option<&str>, value: Option<&Value>) -> Result<(), String> {
        match path {
            // Pathless op: `value` is a partial resource to merge in.
            None => {
                let obj = value.and_then(Value::as_object).ok_or_else(|| {
                    "PATCH value must be an object when no path is given".to_string()
                })?;
                for (k, v) in obj {
                    self.set_attr(k, v);
                }
                Ok(())
            }
            Some(p) => {
                let v = value.ok_or_else(|| format!("PATCH replace of '{p}' requires a value"))?;
                self.set_attr(p, v);
                Ok(())
            }
        }
    }

    fn set_attr(&mut self, path: &str, v: &Value) {
        match path.to_ascii_lowercase().as_str() {
            "active" => self.active = parse_bool_value(v),
            "username" => {
                if let Some(s) = v.as_str() {
                    self.user_name = s.trim().to_string();
                }
            }
            "displayname" => self.display_name = opt_str(v),
            "externalid" => self.external_id = opt_str(v),
            "name.givenname" => self.given_name = opt_str(v),
            "name.familyname" => self.family_name = opt_str(v),
            "name" => {
                if let Some(n) = v.as_object() {
                    self.given_name = n.get("givenName").and_then(opt_str_ref);
                    self.family_name = n.get("familyName").and_then(opt_str_ref);
                }
            }
            "emails" => self.emails = parse_emails(Some(v)),
            _ => {} // unknown attribute: ignore
        }
    }

    fn remove(&mut self, path: Option<&str>) {
        match path.map(|p| p.to_ascii_lowercase()) {
            Some(p) if p == "active" => self.active = false,
            Some(p) if p == "displayname" => self.display_name = None,
            Some(p) if p == "externalid" => self.external_id = None,
            Some(p) if p == "name.givenname" => self.given_name = None,
            Some(p) if p == "name.familyname" => self.family_name = None,
            Some(p) if p == "emails" => self.emails.clear(),
            _ => {}
        }
    }
}

/// A provisioned SCIM group. Membership references users by `id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScimGroup {
    pub id: String,
    pub display_name: String,
    pub external_id: Option<String>,
    pub members: Vec<String>,
    pub created: String,
    pub last_modified: String,
}

impl ScimGroup {
    fn from_resource(v: &Value) -> Result<ScimGroup, String> {
        let display_name =
            string_field(v, "displayName").ok_or_else(|| "displayName is required".to_string())?;
        Ok(ScimGroup {
            id: String::new(),
            display_name,
            external_id: string_field(v, "externalId"),
            members: parse_members(v.get("members")),
            created: String::new(),
            last_modified: String::new(),
        })
    }

    fn to_resource(&self) -> Value {
        let members: Vec<Value> = self
            .members
            .iter()
            .map(|m| json!({"value": m, "type": "User"}))
            .collect();
        let mut obj = json!({
            "schemas": [SCHEMA_GROUP],
            "id": self.id,
            "displayName": self.display_name,
            "meta": {
                "resourceType": "Group",
                "created": self.created,
                "lastModified": self.last_modified,
                "location": format!("/scim/v2/Groups/{}", self.id),
            }
        });
        if let Some(e) = &self.external_id {
            obj["externalId"] = json!(e);
        }
        if !members.is_empty() {
            obj["members"] = json!(members);
        }
        obj
    }

    fn apply_patch(&mut self, patch: &Value) -> Result<(), String> {
        let ops = patch
            .get("Operations")
            .or_else(|| patch.get("operations"))
            .and_then(Value::as_array)
            .ok_or_else(|| "PATCH body requires an Operations array".to_string())?;
        for op in ops {
            let name = op
                .get("op")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            let path = op.get("path").and_then(Value::as_str).map(str::trim);
            let value = op.get("value");
            match name.as_str() {
                "add" => self.merge(path, value, true),
                "replace" => self.merge(path, value, false),
                "remove" => {
                    // We support clearing the membership list; member-filter
                    // expressions (`members[value eq "x"]`) are not parsed.
                    if matches!(path.map(|p| p.to_ascii_lowercase()), Some(p) if p == "members") {
                        self.members.clear();
                    }
                }
                other => return Err(format!("unsupported PATCH op '{other}'")),
            }
        }
        Ok(())
    }

    fn merge(&mut self, path: Option<&str>, value: Option<&Value>, append: bool) {
        match path.map(|p| p.to_ascii_lowercase()) {
            Some(p) if p == "displayname" => {
                if let Some(s) = value.and_then(Value::as_str) {
                    self.display_name = s.trim().to_string();
                }
            }
            Some(p) if p == "externalid" => self.external_id = value.and_then(opt_str_ref),
            Some(p) if p == "members" => {
                let mut m = parse_members(value);
                if append {
                    self.members.append(&mut m);
                } else {
                    self.members = m;
                }
            }
            None => {
                if let Some(obj) = value.and_then(Value::as_object) {
                    if let Some(d) = obj.get("displayName").and_then(Value::as_str) {
                        self.display_name = d.trim().to_string();
                    }
                    if let Some(e) = obj.get("externalId") {
                        self.external_id = opt_str(e);
                    }
                    if let Some(m) = obj.get("members") {
                        self.members = parse_members(Some(m));
                    }
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Resource-parsing helpers
// ---------------------------------------------------------------------------

/// Read a trimmed, non-empty string attribute.
fn string_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A string value for PATCH: `Some` for a non-empty string, `None` otherwise
/// (so `null`/empty clears an optional attribute).
fn opt_str(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn opt_str_ref(v: &Value) -> Option<String> {
    opt_str(v)
}

/// `true` for JSON `true`/`"true"`/`"1"`, `false` otherwise. IdPs sometimes send
/// booleans as strings.
fn parse_bool_value(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::String(s) => matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "1"),
        _ => false,
    }
}

/// Extract email values from a SCIM `emails` array (objects with `value`, or
/// bare strings). A `primary: true` entry is moved to the front.
fn parse_emails(v: Option<&Value>) -> Vec<String> {
    let Some(arr) = v.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut primary = None;
    let mut rest = Vec::new();
    for item in arr {
        let (value, is_primary) = match item {
            Value::String(s) => (Some(s.trim().to_string()), false),
            Value::Object(o) => (
                o.get("value")
                    .and_then(Value::as_str)
                    .map(|s| s.trim().to_string()),
                o.get("primary").map(parse_bool_value).unwrap_or(false),
            ),
            _ => (None, false),
        };
        if let Some(value) = value.filter(|s| !s.is_empty()) {
            if is_primary && primary.is_none() {
                primary = Some(value);
            } else {
                rest.push(value);
            }
        }
    }
    let mut out = Vec::new();
    if let Some(p) = primary {
        out.push(p);
    }
    out.extend(rest);
    out
}

/// Extract member `id`s from a SCIM `members` array (objects with `value`, or
/// bare strings).
fn parse_members(v: Option<&Value>) -> Vec<String> {
    let Some(arr) = v.and_then(Value::as_array) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| match item {
            Value::String(s) => Some(s.trim().to_string()),
            Value::Object(o) => o
                .get("value")
                .and_then(Value::as_str)
                .map(|s| s.trim().to_string()),
            _ => None,
        })
        .filter(|s| !s.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

/// Persistence for SCIM resources. The running server uses [`PgStore`];
/// [`MemoryStore`] backs the unit tests.
pub trait ScimStore {
    fn create_user(&self, user: &ScimUser) -> Result<()>;
    fn get_user(&self, id: &str) -> Result<Option<ScimUser>>;
    fn find_user_by_username(&self, user_name: &str) -> Result<Option<ScimUser>>;
    fn list_users(&self) -> Result<Vec<ScimUser>>;
    /// Update an existing user. Returns `false` if no user had that `id`.
    fn put_user(&self, user: &ScimUser) -> Result<bool>;
    /// Delete a user. Returns `false` if no user had that `id`.
    fn delete_user(&self, id: &str) -> Result<bool>;

    fn create_group(&self, group: &ScimGroup) -> Result<()>;
    fn get_group(&self, id: &str) -> Result<Option<ScimGroup>>;
    fn list_groups(&self) -> Result<Vec<ScimGroup>>;
    fn put_group(&self, group: &ScimGroup) -> Result<bool>;
    fn delete_group(&self, id: &str) -> Result<bool>;
}

/// In-memory store used by tests (and a usable reference implementation). State
/// is lost on restart, so it is **not** wired into the running server.
#[derive(Default)]
pub struct MemoryStore {
    users: Mutex<HashMap<String, ScimUser>>,
    groups: Mutex<HashMap<String, ScimGroup>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ScimStore for MemoryStore {
    fn create_user(&self, user: &ScimUser) -> Result<()> {
        self.users
            .lock()
            .unwrap()
            .insert(user.id.clone(), user.clone());
        Ok(())
    }
    fn get_user(&self, id: &str) -> Result<Option<ScimUser>> {
        Ok(self.users.lock().unwrap().get(id).cloned())
    }
    fn find_user_by_username(&self, user_name: &str) -> Result<Option<ScimUser>> {
        Ok(self
            .users
            .lock()
            .unwrap()
            .values()
            .find(|u| u.user_name.eq_ignore_ascii_case(user_name))
            .cloned())
    }
    fn list_users(&self) -> Result<Vec<ScimUser>> {
        let mut v: Vec<ScimUser> = self.users.lock().unwrap().values().cloned().collect();
        v.sort_by(|a, b| a.created.cmp(&b.created).then(a.id.cmp(&b.id)));
        Ok(v)
    }
    fn put_user(&self, user: &ScimUser) -> Result<bool> {
        let mut users = self.users.lock().unwrap();
        if !users.contains_key(&user.id) {
            return Ok(false);
        }
        users.insert(user.id.clone(), user.clone());
        Ok(true)
    }
    fn delete_user(&self, id: &str) -> Result<bool> {
        Ok(self.users.lock().unwrap().remove(id).is_some())
    }

    fn create_group(&self, group: &ScimGroup) -> Result<()> {
        self.groups
            .lock()
            .unwrap()
            .insert(group.id.clone(), group.clone());
        Ok(())
    }
    fn get_group(&self, id: &str) -> Result<Option<ScimGroup>> {
        Ok(self.groups.lock().unwrap().get(id).cloned())
    }
    fn list_groups(&self) -> Result<Vec<ScimGroup>> {
        let mut v: Vec<ScimGroup> = self.groups.lock().unwrap().values().cloned().collect();
        v.sort_by(|a, b| a.created.cmp(&b.created).then(a.id.cmp(&b.id)));
        Ok(v)
    }
    fn put_group(&self, group: &ScimGroup) -> Result<bool> {
        let mut groups = self.groups.lock().unwrap();
        if !groups.contains_key(&group.id) {
            return Ok(false);
        }
        groups.insert(group.id.clone(), group.clone());
        Ok(true)
    }
    fn delete_group(&self, id: &str) -> Result<bool> {
        Ok(self.groups.lock().unwrap().remove(id).is_some())
    }
}

/// Postgres-backed store. Resources live in two dedicated tables in the
/// operator's own database, so provisioning is durable and is the system of
/// record the IdP reconciles against. A fresh connection is opened per call,
/// matching the rest of `serve`'s blocking, per-request model.
pub struct PgStore<'a> {
    config: &'a Config,
}

impl<'a> PgStore<'a> {
    pub fn new(config: &'a Config) -> Self {
        Self { config }
    }

    fn client(&self) -> Result<postgres::Client> {
        let mut client = database::connect(self.config)?;
        ensure_tables(&mut client)?;
        Ok(client)
    }
}

/// Create the SCIM tables if they do not exist (idempotent). Run lazily on each
/// connection so enabling SCIM needs no separate migration step.
fn ensure_tables(client: &mut postgres::Client) -> Result<()> {
    client
        .batch_execute(
            "CREATE TABLE IF NOT EXISTS beskar_scim_users (
                id TEXT PRIMARY KEY,
                user_name TEXT NOT NULL UNIQUE,
                active BOOLEAN NOT NULL DEFAULT TRUE,
                external_id TEXT,
                display_name TEXT,
                given_name TEXT,
                family_name TEXT,
                emails TEXT NOT NULL DEFAULT '[]',
                created TEXT NOT NULL,
                last_modified TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS beskar_scim_groups (
                id TEXT PRIMARY KEY,
                display_name TEXT NOT NULL,
                external_id TEXT,
                members TEXT NOT NULL DEFAULT '[]',
                created TEXT NOT NULL,
                last_modified TEXT NOT NULL
            );",
        )
        .context("failed to ensure SCIM tables exist")
}

const USER_COLS: &str =
    "id, user_name, active, external_id, display_name, given_name, family_name, emails, created, last_modified";

fn row_to_user(row: &Row) -> Result<ScimUser> {
    let emails: String = row.get("emails");
    Ok(ScimUser {
        id: row.get("id"),
        user_name: row.get("user_name"),
        active: row.get("active"),
        external_id: row.get("external_id"),
        display_name: row.get("display_name"),
        given_name: row.get("given_name"),
        family_name: row.get("family_name"),
        emails: serde_json::from_str(&emails).unwrap_or_default(),
        created: row.get("created"),
        last_modified: row.get("last_modified"),
    })
}

fn row_to_group(row: &Row) -> Result<ScimGroup> {
    let members: String = row.get("members");
    Ok(ScimGroup {
        id: row.get("id"),
        display_name: row.get("display_name"),
        external_id: row.get("external_id"),
        members: serde_json::from_str(&members).unwrap_or_default(),
        created: row.get("created"),
        last_modified: row.get("last_modified"),
    })
}

impl ScimStore for PgStore<'_> {
    fn create_user(&self, u: &ScimUser) -> Result<()> {
        let emails = serde_json::to_string(&u.emails)?;
        self.client()?
            .execute(
                "INSERT INTO beskar_scim_users \
                 (id, user_name, active, external_id, display_name, given_name, family_name, emails, created, last_modified) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
                &[
                    &u.id, &u.user_name, &u.active, &u.external_id, &u.display_name,
                    &u.given_name, &u.family_name, &emails, &u.created, &u.last_modified,
                ],
            )
            .context("failed to insert SCIM user")?;
        Ok(())
    }
    fn get_user(&self, id: &str) -> Result<Option<ScimUser>> {
        let sql = format!("SELECT {USER_COLS} FROM beskar_scim_users WHERE id = $1");
        let row = self
            .client()?
            .query_opt(sql.as_str(), &[&id])
            .context("failed to read SCIM user")?;
        row.map(|r| row_to_user(&r)).transpose()
    }
    fn find_user_by_username(&self, user_name: &str) -> Result<Option<ScimUser>> {
        let sql = format!("SELECT {USER_COLS} FROM beskar_scim_users WHERE user_name = $1");
        let row = self
            .client()?
            .query_opt(sql.as_str(), &[&user_name])
            .context("failed to look up SCIM user by userName")?;
        row.map(|r| row_to_user(&r)).transpose()
    }
    fn list_users(&self) -> Result<Vec<ScimUser>> {
        let sql = format!("SELECT {USER_COLS} FROM beskar_scim_users ORDER BY created, id");
        let rows = self
            .client()?
            .query(sql.as_str(), &[])
            .context("failed to list SCIM users")?;
        rows.iter().map(row_to_user).collect()
    }
    fn put_user(&self, u: &ScimUser) -> Result<bool> {
        let emails = serde_json::to_string(&u.emails)?;
        let n = self
            .client()?
            .execute(
                "UPDATE beskar_scim_users SET \
                 user_name=$2, active=$3, external_id=$4, display_name=$5, given_name=$6, \
                 family_name=$7, emails=$8, last_modified=$9 WHERE id=$1",
                &[
                    &u.id,
                    &u.user_name,
                    &u.active,
                    &u.external_id,
                    &u.display_name,
                    &u.given_name,
                    &u.family_name,
                    &emails,
                    &u.last_modified,
                ],
            )
            .context("failed to update SCIM user")?;
        Ok(n > 0)
    }
    fn delete_user(&self, id: &str) -> Result<bool> {
        let n = self
            .client()?
            .execute("DELETE FROM beskar_scim_users WHERE id = $1", &[&id])
            .context("failed to delete SCIM user")?;
        Ok(n > 0)
    }

    fn create_group(&self, g: &ScimGroup) -> Result<()> {
        let members = serde_json::to_string(&g.members)?;
        self.client()?
            .execute(
                "INSERT INTO beskar_scim_groups \
                 (id, display_name, external_id, members, created, last_modified) \
                 VALUES ($1,$2,$3,$4,$5,$6)",
                &[
                    &g.id,
                    &g.display_name,
                    &g.external_id,
                    &members,
                    &g.created,
                    &g.last_modified,
                ],
            )
            .context("failed to insert SCIM group")?;
        Ok(())
    }
    fn get_group(&self, id: &str) -> Result<Option<ScimGroup>> {
        let row = self
            .client()?
            .query_opt(
                "SELECT id, display_name, external_id, members, created, last_modified \
                 FROM beskar_scim_groups WHERE id = $1",
                &[&id],
            )
            .context("failed to read SCIM group")?;
        row.map(|r| row_to_group(&r)).transpose()
    }
    fn list_groups(&self) -> Result<Vec<ScimGroup>> {
        let rows = self
            .client()?
            .query(
                "SELECT id, display_name, external_id, members, created, last_modified \
                 FROM beskar_scim_groups ORDER BY created, id",
                &[],
            )
            .context("failed to list SCIM groups")?;
        rows.iter().map(row_to_group).collect()
    }
    fn put_group(&self, g: &ScimGroup) -> Result<bool> {
        let members = serde_json::to_string(&g.members)?;
        let n = self
            .client()?
            .execute(
                "UPDATE beskar_scim_groups SET \
                 display_name=$2, external_id=$3, members=$4, last_modified=$5 WHERE id=$1",
                &[
                    &g.id,
                    &g.display_name,
                    &g.external_id,
                    &members,
                    &g.last_modified,
                ],
            )
            .context("failed to update SCIM group")?;
        Ok(n > 0)
    }
    fn delete_group(&self, id: &str) -> Result<bool> {
        let n = self
            .client()?
            .execute("DELETE FROM beskar_scim_groups WHERE id = $1", &[&id])
            .context("failed to delete SCIM group")?;
        Ok(n > 0)
    }
}

// ---------------------------------------------------------------------------
// HTTP request handling
// ---------------------------------------------------------------------------

/// Route a SCIM request to the right handler. `method` is the upper-case HTTP
/// verb, `path` is the request path (query stripped), `query` the raw query
/// string, and `body` the request body. Returns `(status, json)`; a `204` body
/// is empty and should not be sent.
pub fn handle(
    store: &dyn ScimStore,
    method: &str,
    path: &str,
    query: Option<&str>,
    body: &str,
) -> (u16, Value) {
    let rest = path
        .strip_prefix("/scim/v2")
        .unwrap_or("")
        .trim_matches('/');
    let mut segs = rest.splitn(2, '/');
    let resource = segs.next().unwrap_or("");
    let id = segs.next().filter(|s| !s.is_empty());

    match (resource, id, method) {
        ("ServiceProviderConfig", None, "GET") => (200, service_provider_config()),

        ("Users", None, "GET") => list_users(store, query),
        ("Users", None, "POST") => create_user(store, body),
        ("Users", Some(id), "GET") => get_user(store, id),
        ("Users", Some(id), "PUT") => replace_user(store, id, body),
        ("Users", Some(id), "PATCH") => patch_user(store, id, body),
        ("Users", Some(id), "DELETE") => delete_user(store, id),

        ("Groups", None, "GET") => list_groups(store, query),
        ("Groups", None, "POST") => create_group(store, body),
        ("Groups", Some(id), "GET") => get_group(store, id),
        ("Groups", Some(id), "PUT") => replace_group(store, id, body),
        ("Groups", Some(id), "PATCH") => patch_group(store, id, body),
        ("Groups", Some(id), "DELETE") => delete_group(store, id),

        ("Users" | "Groups", _, _) => method_not_allowed(),
        _ => (404, error(404, "unknown SCIM endpoint", None)),
    }
}

// --- Users ---

fn create_user(store: &dyn ScimStore, body: &str) -> (u16, Value) {
    let v = match parse_body(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let mut user = match ScimUser::from_resource(&v) {
        Ok(u) => u,
        Err(e) => return bad_request(&e),
    };
    match store.find_user_by_username(&user.user_name) {
        Ok(Some(_)) => {
            return (
                409,
                error(
                    409,
                    &format!("userName '{}' already exists", user.user_name),
                    Some("uniqueness"),
                ),
            )
        }
        Ok(None) => {}
        Err(e) => return server_error(&e),
    }
    let id = match new_id() {
        Ok(id) => id,
        Err(e) => return (500, error(500, &e, None)),
    };
    let now = crate::audit::now_rfc3339();
    user.id = id;
    user.created = now.clone();
    user.last_modified = now;
    match store.create_user(&user) {
        Ok(()) => (201, user.to_resource()),
        Err(e) => server_error(&e),
    }
}

fn get_user(store: &dyn ScimStore, id: &str) -> (u16, Value) {
    match store.get_user(id) {
        Ok(Some(u)) => (200, u.to_resource()),
        Ok(None) => user_not_found(id),
        Err(e) => server_error(&e),
    }
}

fn replace_user(store: &dyn ScimStore, id: &str, body: &str) -> (u16, Value) {
    let v = match parse_body(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let existing = match store.get_user(id) {
        Ok(Some(u)) => u,
        Ok(None) => return user_not_found(id),
        Err(e) => return server_error(&e),
    };
    let mut updated = match ScimUser::from_resource(&v) {
        Ok(u) => u,
        Err(e) => return bad_request(&e),
    };
    // A userName change must not collide with another user.
    if !updated.user_name.eq_ignore_ascii_case(&existing.user_name) {
        match store.find_user_by_username(&updated.user_name) {
            Ok(Some(other)) if other.id != existing.id => {
                return (
                    409,
                    error(
                        409,
                        &format!("userName '{}' already exists", updated.user_name),
                        Some("uniqueness"),
                    ),
                )
            }
            Err(e) => return server_error(&e),
            _ => {}
        }
    }
    updated.id = existing.id;
    updated.created = existing.created;
    updated.last_modified = crate::audit::now_rfc3339();
    match store.put_user(&updated) {
        Ok(true) => (200, updated.to_resource()),
        Ok(false) => user_not_found(id),
        Err(e) => server_error(&e),
    }
}

fn patch_user(store: &dyn ScimStore, id: &str, body: &str) -> (u16, Value) {
    let v = match parse_body(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let mut user = match store.get_user(id) {
        Ok(Some(u)) => u,
        Ok(None) => return user_not_found(id),
        Err(e) => return server_error(&e),
    };
    if let Err(e) = user.apply_patch(&v) {
        return bad_request(&e);
    }
    user.last_modified = crate::audit::now_rfc3339();
    match store.put_user(&user) {
        Ok(true) => (200, user.to_resource()),
        Ok(false) => user_not_found(id),
        Err(e) => server_error(&e),
    }
}

fn delete_user(store: &dyn ScimStore, id: &str) -> (u16, Value) {
    match store.delete_user(id) {
        Ok(true) => (204, Value::Null),
        Ok(false) => user_not_found(id),
        Err(e) => server_error(&e),
    }
}

fn list_users(store: &dyn ScimStore, query: Option<&str>) -> (u16, Value) {
    let users = match store.list_users() {
        Ok(u) => u,
        Err(e) => return server_error(&e),
    };
    let filter = filter_from_query(query);
    let matched: Vec<ScimUser> = match &filter {
        Some(f) => users.into_iter().filter(|u| user_matches(u, f)).collect(),
        None => users,
    };
    let (start, count) = paging(query);
    let total = matched.len();
    let page: Vec<Value> = matched
        .into_iter()
        .skip(start.saturating_sub(1))
        .take(count)
        .map(|u| u.to_resource())
        .collect();
    (200, list_response(total, start, page))
}

fn user_matches(u: &ScimUser, (attr, value): &(String, String)) -> bool {
    match attr.as_str() {
        "username" => u.user_name.eq_ignore_ascii_case(value),
        "externalid" => u.external_id.as_deref() == Some(value.as_str()),
        "id" => u.id == *value,
        _ => false,
    }
}

fn user_not_found(id: &str) -> (u16, Value) {
    (404, error(404, &format!("user '{id}' not found"), None))
}

// --- Groups ---

fn create_group(store: &dyn ScimStore, body: &str) -> (u16, Value) {
    let v = match parse_body(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let mut group = match ScimGroup::from_resource(&v) {
        Ok(g) => g,
        Err(e) => return bad_request(&e),
    };
    let id = match new_id() {
        Ok(id) => id,
        Err(e) => return (500, error(500, &e, None)),
    };
    let now = crate::audit::now_rfc3339();
    group.id = id;
    group.created = now.clone();
    group.last_modified = now;
    match store.create_group(&group) {
        Ok(()) => (201, group.to_resource()),
        Err(e) => server_error(&e),
    }
}

fn get_group(store: &dyn ScimStore, id: &str) -> (u16, Value) {
    match store.get_group(id) {
        Ok(Some(g)) => (200, g.to_resource()),
        Ok(None) => group_not_found(id),
        Err(e) => server_error(&e),
    }
}

fn replace_group(store: &dyn ScimStore, id: &str, body: &str) -> (u16, Value) {
    let v = match parse_body(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let existing = match store.get_group(id) {
        Ok(Some(g)) => g,
        Ok(None) => return group_not_found(id),
        Err(e) => return server_error(&e),
    };
    let mut updated = match ScimGroup::from_resource(&v) {
        Ok(g) => g,
        Err(e) => return bad_request(&e),
    };
    updated.id = existing.id;
    updated.created = existing.created;
    updated.last_modified = crate::audit::now_rfc3339();
    match store.put_group(&updated) {
        Ok(true) => (200, updated.to_resource()),
        Ok(false) => group_not_found(id),
        Err(e) => server_error(&e),
    }
}

fn patch_group(store: &dyn ScimStore, id: &str, body: &str) -> (u16, Value) {
    let v = match parse_body(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let mut group = match store.get_group(id) {
        Ok(Some(g)) => g,
        Ok(None) => return group_not_found(id),
        Err(e) => return server_error(&e),
    };
    if let Err(e) = group.apply_patch(&v) {
        return bad_request(&e);
    }
    group.last_modified = crate::audit::now_rfc3339();
    match store.put_group(&group) {
        Ok(true) => (200, group.to_resource()),
        Ok(false) => group_not_found(id),
        Err(e) => server_error(&e),
    }
}

fn delete_group(store: &dyn ScimStore, id: &str) -> (u16, Value) {
    match store.delete_group(id) {
        Ok(true) => (204, Value::Null),
        Ok(false) => group_not_found(id),
        Err(e) => server_error(&e),
    }
}

fn list_groups(store: &dyn ScimStore, query: Option<&str>) -> (u16, Value) {
    let groups = match store.list_groups() {
        Ok(g) => g,
        Err(e) => return server_error(&e),
    };
    let filter = filter_from_query(query);
    let matched: Vec<ScimGroup> = match &filter {
        Some(f) => groups.into_iter().filter(|g| group_matches(g, f)).collect(),
        None => groups,
    };
    let (start, count) = paging(query);
    let total = matched.len();
    let page: Vec<Value> = matched
        .into_iter()
        .skip(start.saturating_sub(1))
        .take(count)
        .map(|g| g.to_resource())
        .collect();
    (200, list_response(total, start, page))
}

fn group_matches(g: &ScimGroup, (attr, value): &(String, String)) -> bool {
    match attr.as_str() {
        "displayname" => g.display_name.eq_ignore_ascii_case(value),
        "externalid" => g.external_id.as_deref() == Some(value.as_str()),
        "id" => g.id == *value,
        _ => false,
    }
}

fn group_not_found(id: &str) -> (u16, Value) {
    (404, error(404, &format!("group '{id}' not found"), None))
}

// ---------------------------------------------------------------------------
// Response/parse helpers
// ---------------------------------------------------------------------------

fn parse_body(body: &str) -> Result<Value, (u16, Value)> {
    serde_json::from_str(body).map_err(|e| bad_request(&format!("invalid JSON: {e}")))
}

fn bad_request(detail: &str) -> (u16, Value) {
    (400, error(400, detail, Some("invalidValue")))
}

fn method_not_allowed() -> (u16, Value) {
    (
        405,
        error(405, "method not allowed for this SCIM resource", None),
    )
}

/// Map a core error to a redacted SCIM 500 (secrets scrubbed via the E1.3 registry).
fn server_error(e: &anyhow::Error) -> (u16, Value) {
    (500, error(500, &secrets::redact(&format!("{e:#}")), None))
}

fn error(status: u16, detail: &str, scim_type: Option<&str>) -> Value {
    let mut e = json!({
        "schemas": [SCHEMA_ERROR],
        "status": status.to_string(),
        "detail": detail,
    });
    if let Some(t) = scim_type {
        e["scimType"] = json!(t);
    }
    e
}

fn list_response(total: usize, start_index: usize, resources: Vec<Value>) -> Value {
    json!({
        "schemas": [SCHEMA_LIST],
        "totalResults": total,
        "startIndex": start_index,
        "itemsPerPage": resources.len(),
        "Resources": resources,
    })
}

/// Minimal SCIM ServiceProviderConfig so IdPs can discover capabilities.
fn service_provider_config() -> Value {
    json!({
        "schemas": [SCHEMA_SPC],
        "documentationUri": "https://github.com/Mandoa-Labs/beskar/blob/main/docs/scim.md",
        "patch": {"supported": true},
        "bulk": {"supported": false, "maxOperations": 0, "maxPayloadSize": 0},
        "filter": {"supported": true, "maxResults": 200},
        "changePassword": {"supported": false},
        "sort": {"supported": false},
        "etag": {"supported": false},
        "authenticationSchemes": [{
            "type": "oauthbearertoken",
            "name": "OAuth Bearer Token",
            "description": "Authentication via the `beskar serve` bearer token.",
            "primary": true
        }],
        "meta": {"resourceType": "ServiceProviderConfig", "location": "/scim/v2/ServiceProviderConfig"}
    })
}

/// Generate a random 128-bit hex resource id (uses the OpenSSL CSPRNG, which is
/// the FIPS-validated module on a FIPS build).
fn new_id() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    openssl::rand::rand_bytes(&mut bytes).map_err(|e| format!("failed to generate id: {e}"))?;
    Ok(hex::encode(bytes))
}

/// Parse a `(lowercased-attribute, value)` equality filter from the `filter`
/// query parameter, supporting the `<attr> eq "<value>"` form IdPs use to
/// look a resource up before provisioning it.
fn filter_from_query(query: Option<&str>) -> Option<(String, String)> {
    parse_filter(&query_param(query, "filter")?)
}

fn parse_filter(expr: &str) -> Option<(String, String)> {
    let expr = expr.trim();
    let idx = expr.to_ascii_lowercase().find(" eq ")?;
    let attr = expr[..idx].trim();
    let value = expr[idx + 4..].trim().trim_matches('"');
    if attr.is_empty() {
        return None;
    }
    Some((attr.to_ascii_lowercase(), value.to_string()))
}

/// `(startIndex, count)` from the query, 1-based per RFC 7644; defaults return
/// the whole collection.
fn paging(query: Option<&str>) -> (usize, usize) {
    let start = query_param(query, "startIndex")
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n >= 1)
        .unwrap_or(1);
    let count = query_param(query, "count")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(usize::MAX);
    (start, count)
}

/// Look up a query-string parameter (case-insensitive key), percent-decoding the
/// value. Avoids a urlencoding dependency.
fn query_param(query: Option<&str>, key: &str) -> Option<String> {
    for pair in query?.split('&') {
        let mut it = pair.splitn(2, '=');
        let k = it.next().unwrap_or("");
        if k.eq_ignore_ascii_case(key) {
            return Some(percent_decode(it.next().unwrap_or("")));
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h * 16 + l);
                    i += 3;
                }
                _ => {
                    out.push(bytes[i]);
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_body(user_name: &str) -> String {
        json!({
            "schemas": [SCHEMA_USER],
            "userName": user_name,
            "name": {"givenName": "Ada", "familyName": "Lovelace"},
            "emails": [{"value": user_name, "primary": true}],
            "externalId": "ext-123"
        })
        .to_string()
    }

    fn created_user_id(store: &MemoryStore, user_name: &str) -> String {
        let (code, v) = handle(store, "POST", "/scim/v2/Users", None, &user_body(user_name));
        assert_eq!(code, 201, "create failed: {v}");
        v["id"].as_str().unwrap().to_string()
    }

    #[test]
    fn create_provisions_user_with_id_and_meta() {
        let store = MemoryStore::new();
        let (code, v) = handle(
            store_ref(&store),
            "POST",
            "/scim/v2/Users",
            None,
            &user_body("ada@x.com"),
        );
        assert_eq!(code, 201);
        assert_eq!(v["schemas"][0], SCHEMA_USER);
        assert!(v["id"].as_str().is_some_and(|s| !s.is_empty()));
        assert_eq!(v["userName"], "ada@x.com");
        assert_eq!(v["active"], true);
        assert_eq!(v["meta"]["resourceType"], "User");
        assert_eq!(v["emails"][0]["value"], "ada@x.com");
        assert_eq!(v["name"]["familyName"], "Lovelace");
    }

    #[test]
    fn get_returns_provisioned_user() {
        let store = MemoryStore::new();
        let id = created_user_id(&store, "ada@x.com");
        let (code, v) = handle(
            store_ref(&store),
            "GET",
            &format!("/scim/v2/Users/{id}"),
            None,
            "",
        );
        assert_eq!(code, 200);
        assert_eq!(v["id"], id);
    }

    #[test]
    fn duplicate_username_is_conflict() {
        let store = MemoryStore::new();
        created_user_id(&store, "ada@x.com");
        let (code, v) = handle(
            store_ref(&store),
            "POST",
            "/scim/v2/Users",
            None,
            &user_body("ada@x.com"),
        );
        assert_eq!(code, 409);
        assert_eq!(v["scimType"], "uniqueness");
    }

    #[test]
    fn patch_active_false_deactivates_user() {
        let store = MemoryStore::new();
        let id = created_user_id(&store, "ada@x.com");
        let patch = json!({
            "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
            "Operations": [{"op": "Replace", "path": "active", "value": false}]
        })
        .to_string();
        let (code, v) = handle(
            store_ref(&store),
            "PATCH",
            &format!("/scim/v2/Users/{id}"),
            None,
            &patch,
        );
        assert_eq!(code, 200);
        assert_eq!(v["active"], false);
        // Persisted.
        assert!(!store.get_user(&id).unwrap().unwrap().active);
    }

    #[test]
    fn patch_pathless_object_merge_deactivates() {
        let store = MemoryStore::new();
        let id = created_user_id(&store, "ada@x.com");
        // Microsoft Entra ID's pathless form.
        let patch = json!({
            "Operations": [{"op": "replace", "value": {"active": false}}]
        })
        .to_string();
        let (code, v) = handle(
            store_ref(&store),
            "PATCH",
            &format!("/scim/v2/Users/{id}"),
            None,
            &patch,
        );
        assert_eq!(code, 200);
        assert_eq!(v["active"], false);
    }

    #[test]
    fn delete_deprovisions_user() {
        let store = MemoryStore::new();
        let id = created_user_id(&store, "ada@x.com");
        let (code, _) = handle(
            store_ref(&store),
            "DELETE",
            &format!("/scim/v2/Users/{id}"),
            None,
            "",
        );
        assert_eq!(code, 204);
        let (code, _) = handle(
            store_ref(&store),
            "GET",
            &format!("/scim/v2/Users/{id}"),
            None,
            "",
        );
        assert_eq!(code, 404);
    }

    #[test]
    fn put_replaces_attributes() {
        let store = MemoryStore::new();
        let id = created_user_id(&store, "ada@x.com");
        let body = json!({"schemas": [SCHEMA_USER], "userName": "ada@x.com", "active": false, "displayName": "Ada L."}).to_string();
        let (code, v) = handle(
            store_ref(&store),
            "PUT",
            &format!("/scim/v2/Users/{id}"),
            None,
            &body,
        );
        assert_eq!(code, 200);
        assert_eq!(v["active"], false);
        assert_eq!(v["displayName"], "Ada L.");
    }

    #[test]
    fn list_filters_by_username() {
        let store = MemoryStore::new();
        created_user_id(&store, "ada@x.com");
        created_user_id(&store, "grace@x.com");
        let (code, v) = handle(
            store_ref(&store),
            "GET",
            "/scim/v2/Users",
            Some("filter=userName eq \"grace@x.com\""),
            "",
        );
        assert_eq!(code, 200);
        assert_eq!(v["totalResults"], 1);
        assert_eq!(v["Resources"][0]["userName"], "grace@x.com");
    }

    #[test]
    fn list_without_filter_returns_all() {
        let store = MemoryStore::new();
        created_user_id(&store, "ada@x.com");
        created_user_id(&store, "grace@x.com");
        let (code, v) = handle(store_ref(&store), "GET", "/scim/v2/Users", None, "");
        assert_eq!(code, 200);
        assert_eq!(v["totalResults"], 2);
        assert_eq!(v["schemas"][0], SCHEMA_LIST);
    }

    #[test]
    fn missing_username_is_bad_request() {
        let store = MemoryStore::new();
        let (code, _) = handle(store_ref(&store), "POST", "/scim/v2/Users", None, "{}");
        assert_eq!(code, 400);
    }

    #[test]
    fn unsupported_method_is_405() {
        let store = MemoryStore::new();
        let (code, _) = handle(store_ref(&store), "PATCH", "/scim/v2/Users", None, "");
        assert_eq!(code, 405);
    }

    #[test]
    fn group_create_get_and_member_patch() {
        let store = MemoryStore::new();
        let uid = created_user_id(&store, "ada@x.com");
        let body = json!({"schemas": [SCHEMA_GROUP], "displayName": "Engineers"}).to_string();
        let (code, v) = handle(store_ref(&store), "POST", "/scim/v2/Groups", None, &body);
        assert_eq!(code, 201);
        let gid = v["id"].as_str().unwrap().to_string();

        let patch = json!({
            "Operations": [{"op": "add", "path": "members", "value": [{"value": uid}]}]
        })
        .to_string();
        let (code, v) = handle(
            store_ref(&store),
            "PATCH",
            &format!("/scim/v2/Groups/{gid}"),
            None,
            &patch,
        );
        assert_eq!(code, 200);
        assert_eq!(v["members"][0]["value"], uid);
    }

    #[test]
    fn service_provider_config_advertises_patch_and_filter() {
        let store = MemoryStore::new();
        let (code, v) = handle(
            store_ref(&store),
            "GET",
            "/scim/v2/ServiceProviderConfig",
            None,
            "",
        );
        assert_eq!(code, 200);
        assert_eq!(v["patch"]["supported"], true);
        assert_eq!(v["filter"]["supported"], true);
    }

    #[test]
    fn parse_filter_handles_quoted_value() {
        assert_eq!(
            parse_filter("userName eq \"a@b.com\""),
            Some(("username".to_string(), "a@b.com".to_string()))
        );
        assert_eq!(parse_filter("garbage"), None);
    }

    #[test]
    fn percent_decode_handles_encoded_filter() {
        assert_eq!(
            percent_decode("userName%20eq%20%22a%40b%22"),
            "userName eq \"a@b\""
        );
        assert_eq!(percent_decode("a+b"), "a b");
    }

    #[test]
    fn parse_emails_orders_primary_first() {
        let v = json!([{"value": "second@x"}, {"value": "first@x", "primary": true}]);
        assert_eq!(
            parse_emails(Some(&v)),
            vec!["first@x".to_string(), "second@x".to_string()]
        );
    }

    // Coerce `&MemoryStore` to `&dyn ScimStore` for the handler.
    fn store_ref(s: &MemoryStore) -> &dyn ScimStore {
        s
    }
}
