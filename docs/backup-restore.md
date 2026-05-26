# Backup & restore (E1.12, §8.4)

A Beskar corpus lives entirely in **your** PostgreSQL database, so it backs up
and restores with the standard PostgreSQL tooling — there is no Beskar-specific
state on disk beyond `~/.config/beskar/config.yaml` (which holds only connection
settings and secret references, no corpus data). This document is the
recommended procedure; after any restore, validate it with
[`beskar db --verify`](#verifying-a-restore).

## What a corpus is

`beskar db --create --table-name X` provisions, in the `public` schema:

| Object | Holds |
| --- | --- |
| `X_documents` | one row per ingested file (filename, source path, content, `content_sha256`) |
| `X_chunks` | one row per chunk (text + `embedding VECTOR`), FK → `X_documents` `ON DELETE CASCADE` |
| `X_meta` | one row recording the embedding `model` + vector `dim` the corpus was built with (E1.5) |
| `X_chunks_embedding_idx` | the pgvector HNSW index used for similarity search |

All of these depend on the **pgvector** extension (`CREATE EXTENSION vector`)
being present in the target database.

## Back up

### Option A — selective dump (one corpus)

Dump just the three tables (and their index definitions) for corpus `X`:

```bash
pg_dump "$PGCONN" \
  -t public.X_documents \
  -t public.X_chunks \
  -t public.X_meta \
  -Fc -f kb-X.dump
```

`-Fc` is the custom (compressed) format, restored with `pg_restore`. For a plain
SQL file use `-Fp -f kb-X.sql` and restore with `psql`.

### Option B — full database dump

```bash
pg_dump "$PGCONN" -Fc -f beskar-all.dump
```

This captures every corpus plus the `vector` extension declaration.

> **Embeddings are the expensive part.** The chunk text and its source files can
> be re-ingested for free, but the embedding vectors cost provider API calls to
> regenerate. Back up `X_chunks` (or re-budget for re-embedding) accordingly.

## Restore

1. **Ensure pgvector exists** in the target database (the vector column type must
   resolve before the tables load):

   ```bash
   psql "$PGCONN_TARGET" -c 'CREATE EXTENSION IF NOT EXISTS vector;'
   ```

2. **Restore the dump:**

   ```bash
   # custom-format dump (Option A or B)
   pg_restore -d "$PGCONN_TARGET" --no-owner kb-X.dump
   # or, for a plain SQL dump
   psql "$PGCONN_TARGET" -f kb-X.sql
   ```

3. **Rebuild the vector index if it didn't come across** (e.g. a data-only or
   table-filtered dump). The name is deterministic:

   ```sql
   CREATE INDEX IF NOT EXISTS X_chunks_embedding_idx
     ON X_chunks USING hnsw (embedding vector_cosine_ops);
   ```

4. **Verify** (next section).

## Verifying a restore

Point `config.yaml` at the restored database and run:

```bash
beskar db --verify --table-name X
```

It checks that all three tables exist, reports document/chunk counts, confirms
the vector index is present, that every chunk references a live document and
carries a non-NULL embedding, and that all embeddings match the dimension
recorded in `X_meta`. It prints a per-check `[PASS]`/`[FAIL]` report and **exits
non-zero if any check fails**, so it works as an automated post-restore gate:

```bash
if beskar db --verify --table-name X; then
  echo "corpus restored cleanly"
else
  echo "restore is incomplete — see the failed checks above" >&2
  exit 1
fi
```

A common post-restore finding is a missing vector index (step 3) — `--verify`
flags it as a `[FAIL]` because similarity queries would otherwise silently fall
back to a sequential scan.

## Re-ingest alternative

Because the source documents plus `config.yaml` fully determine a corpus, you can
also rebuild from scratch instead of restoring vectors:

```bash
beskar db --create --table-name X
beskar document --path ./corpus-sources --table-name X
```

This re-embeds every chunk (incurring provider API cost) but needs no database
backup — useful for disaster recovery when only the source files survive. The
`content_sha256` idempotency check means re-running `document` over unchanged
files is a no-op.
