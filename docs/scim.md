# SCIM provisioning — `beskar serve` (E2.4, §15)

When SCIM is enabled, `beskar serve` exposes the **SCIM 2.0** protocol (RFC 7643 /
7644) under `/scim/v2/*`, so an identity provider (Okta, Microsoft Entra ID,
OneLogin, …) can **provision and deprovision** users and groups automatically:

- creating a user in the IdP → `POST /scim/v2/Users` here (the user is created);
- deactivating a user in the IdP → `PATCH` with `active=false` (the user is
  marked inactive) or `DELETE /scim/v2/Users/{id}` (the user is removed);
- the same lifecycle applies to groups under `/scim/v2/Groups`.

Provisioned resources are stored in **your own Postgres** (the platform's durable
store), in two dedicated tables — `beskar_scim_users` and `beskar_scim_groups` —
created automatically on first use. That makes Beskar the system of record the
IdP reconciles against, and the state survives restarts.

## Enabling it

```yaml
# ~/.config/beskar/config.yaml
scim:
  enabled: true
```

With `scim.enabled: false` (the default) every `/scim/v2/*` route returns `404`,
so the surface is opt-in. `beskar config lint` reports whether SCIM is enabled.

## Authentication

SCIM requests authenticate with the **same bearer token** as the rest of the
server (this is the "SCIM bearer token" you paste into the IdP's provisioning
config):

```
Authorization: Bearer <BESKAR_SERVE_TOKEN>
```

A missing or wrong token returns `401`. Point the IdP at
`https://<your-host>/scim/v2` as the **SCIM base URL** and configure the same
token.

## Endpoints

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/scim/v2/ServiceProviderConfig` | capability discovery |
| `POST` | `/scim/v2/Users` | create (provision) a user |
| `GET` | `/scim/v2/Users` | list / search (`?filter=userName eq "x"`) |
| `GET` | `/scim/v2/Users/{id}` | fetch a user |
| `PUT` | `/scim/v2/Users/{id}` | replace a user |
| `PATCH` | `/scim/v2/Users/{id}` | update a user (e.g. `active=false`) |
| `DELETE` | `/scim/v2/Users/{id}` | delete (deprovision) a user |
| `POST`/`GET`/`GET {id}`/`PUT {id}`/`PATCH {id}`/`DELETE {id}` | `/scim/v2/Groups[/…]` | same lifecycle for groups |

### Create a user

```bash
curl -s https://host/scim/v2/Users \
  -H "Authorization: Bearer $BESKAR_SERVE_TOKEN" \
  -H 'Content-Type: application/scim+json' \
  -d '{
        "schemas": ["urn:ietf:params:scim:schemas:core:2.0:User"],
        "userName": "ada@example.com",
        "name": {"givenName": "Ada", "familyName": "Lovelace"},
        "emails": [{"value": "ada@example.com", "primary": true}],
        "active": true,
        "externalId": "okta-00u123"
      }'
# 201 Created
# {"schemas":[...],"id":"<32-hex>","userName":"ada@example.com","active":true,
#  "name":{...},"emails":[...],"meta":{"resourceType":"User","location":"/scim/v2/Users/<id>",...}}
```

`userName` is required and unique: a duplicate returns `409` with
`"scimType":"uniqueness"`, which is how IdPs detect an already-provisioned user.

### Deactivate (deprovision) a user

Most IdPs **soft-deactivate** with a PATCH:

```bash
curl -s -X PATCH https://host/scim/v2/Users/<id> \
  -H "Authorization: Bearer $BESKAR_SERVE_TOKEN" \
  -H 'Content-Type: application/scim+json' \
  -d '{
        "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
        "Operations": [{"op": "replace", "path": "active", "value": false}]
      }'
# 200 OK — user remains, "active": false
```

The pathless object-merge form Microsoft Entra ID sends is also accepted:
`{"Operations":[{"op":"replace","value":{"active":false}}]}`.

Some IdPs **hard-delete** instead:

```bash
curl -s -X DELETE https://host/scim/v2/Users/<id> \
  -H "Authorization: Bearer $BESKAR_SERVE_TOKEN"
# 204 No Content — user removed
```

### Search before provisioning

IdPs look a user up by `userName` before creating it:

```bash
curl -s -G https://host/scim/v2/Users \
  -H "Authorization: Bearer $BESKAR_SERVE_TOKEN" \
  --data-urlencode 'filter=userName eq "ada@example.com"'
# {"schemas":["urn:ietf:params:scim:api:messages:2.0:ListResponse"],
#  "totalResults":1,"startIndex":1,"itemsPerPage":1,"Resources":[{...}]}
```

Supported filters: `userName eq "…"`, `externalId eq "…"`, `id eq "…"` (and the
group equivalent on `displayName`). `startIndex` / `count` pagination is honored.

## Responses & errors

Errors use the SCIM error schema
(`urn:ietf:params:scim:api:messages:2.0:Error`):

- `200` / `201` / `204` — success.
- `400` — malformed JSON or a missing required attribute (`userName`,
  `displayName`).
- `401` — missing/invalid bearer token.
- `404` — SCIM disabled, unknown route, or unknown resource id.
- `405` — method not allowed on that resource.
- `409` — `userName` uniqueness conflict.
- `500` — a storage error; the message is run through the secret-redaction
  registry (E1.3) before it is returned.

Each SCIM request also emits an audit event (`serve-scim`) through the same sink
as the rest of the CLI/server when `BESKAR_AUDIT_SINK` / `BESKAR_AUDIT_FILE` are
set (E1.8), giving a full provisioning trail.

## Scope & limits

- Core User/Group attributes are modeled; unknown attributes are accepted but not
  persisted (lenient-server behavior, RFC 7644 §3.5.2).
- Group PATCH supports replacing `displayName`/`externalId`, and
  adding/replacing/clearing `members`; member-filter expressions
  (`members[value eq "x"]`) are not parsed.
- `PATCH` covers the operations IdPs use for provisioning; it is not a complete
  PATCH-path-expression implementation.
- The server is single-worker and blocking — put it behind a reverse proxy for
  TLS and concurrency, the same as the rest of [`beskar serve`](server.md).
