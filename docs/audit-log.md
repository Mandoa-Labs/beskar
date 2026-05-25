# Audit log schema (E1.8)

Beskar can emit a structured JSON **audit event** for every security-relevant
action — config init, database create/drop/list, document ingestion, and
query (`generate`). Events are designed for SIEM ingestion: one JSON object per
line, a stable schema, and **no secrets** (the error message on a failed action
is passed through the same redaction registry that scrubs Beskar's stderr).

## Enabling

Auditing is **off by default**. It is configured from the environment so it is
available even for `beskar init`, which runs before any config file exists.

| Variable             | Values / meaning                                                        |
| -------------------- | ----------------------------------------------------------------------- |
| `BESKAR_AUDIT_SINK`  | `off` (default), `stderr`, `file`, or `syslog`.                         |
| `BESKAR_AUDIT_FILE`  | Path for the `file` sink. Setting this alone selects the `file` sink.   |

```bash
# Append one JSON event per action to a file:
export BESKAR_AUDIT_FILE=/var/log/beskar/audit.log
beskar document --path ./runbooks --table-name runbooks

# Or stream to stderr (e.g. captured by systemd-journald):
BESKAR_AUDIT_SINK=stderr beskar generate --query "..." --table-name runbooks

# Or to the local syslog daemon (RFC 5424, facility local0):
BESKAR_AUDIT_SINK=syslog beskar db --create --table-name runbooks
```

Sink-write failures never fail the command: they degrade to a warning on
stderr (and the `syslog` sink falls back to stderr if no socket accepts it), so
auditing can't take down an operation.

## Schema (version `1`)

Each event is a single-line JSON object:

```json
{
  "schema_version": "1",
  "timestamp": "2026-05-25T14:03:11.482Z",
  "actor": "platform-svc",
  "host": "build-runner-07",
  "pid": 48213,
  "command": "document",
  "target": "runbooks",
  "outcome": "success"
}
```

A failed action adds a redacted `error` field:

```json
{
  "schema_version": "1",
  "timestamp": "2026-05-25T14:05:02.119Z",
  "actor": "platform-svc",
  "host": "build-runner-07",
  "pid": 48230,
  "command": "generate",
  "target": "runbooks",
  "outcome": "failure",
  "error": "egress allowlist: 'api.openai.com' is not permitted."
}
```

| Field            | Type    | Always present | Notes                                                                 |
| ---------------- | ------- | -------------- | --------------------------------------------------------------------- |
| `schema_version` | string  | yes            | `"1"`. Bumped only on a breaking change.                              |
| `timestamp`      | string  | yes            | UTC, RFC 3339, millisecond precision, `Z` suffix.                     |
| `actor`          | string  | yes            | OS user (`USER`/`USERNAME`/`LOGNAME`), or `unknown`.                  |
| `host`           | string  | yes            | Hostname (`HOSTNAME`/`COMPUTERNAME`/`/etc/hostname`), or `unknown`.   |
| `pid`            | number  | yes            | Process ID of the `beskar` invocation.                                |
| `command`        | string  | yes            | One of `init`, `config-lint`, `db`, `document`, `generate`.           |
| `target`         | string  | no             | Corpus / `--table-name` when applicable; omitted otherwise.           |
| `outcome`        | string  | yes            | `success` or `failure`.                                               |
| `error`          | string  | no             | Redacted error message; present only when `outcome` is `failure`.     |

Absent optional fields are **omitted** rather than emitted as `null`.

### Secret safety

`error` is run through Beskar's secret-redaction registry before it is written,
so any resolved credential is replaced with `***REDACTED***`. No other field can
contain a secret. This is verified in CI, which runs an audited command with a
sentinel secret and asserts the secret never appears in the emitted events.

## JSON Schema

A machine-readable [JSON Schema (draft 2020-12)](audit-log.schema.json)
accompanies this document for validating events in your pipeline.
