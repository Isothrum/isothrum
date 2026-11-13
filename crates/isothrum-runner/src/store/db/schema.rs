//! The store's schema, as the migrations that build it: each runs once, in order, in the
//! transaction that records it in `schema_migrations`.

/// The migrations, by version from 1. A home whose newest recorded version is past the last is
/// refused (`store_version`): a newer runner wrote it.
pub(super) const MIGRATIONS: &[&str] = &[V1];

/// Version 1: the runs, their transitions, the journal, the events, the resources, the port
/// reservations and the leases.
const V1: &str = r"
CREATE TABLE store_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE runs (
  name            TEXT PRIMARY KEY,
  manifest_path   TEXT NOT NULL,
  manifest_digest TEXT NOT NULL,
  target          TEXT NOT NULL,
  driver          TEXT NOT NULL,
  desired         TEXT NOT NULL,
  state           TEXT NOT NULL,
  failed_cause    TEXT,
  state_op        INTEGER REFERENCES operations(id),
  state_at        TEXT NOT NULL,
  record          TEXT NOT NULL,
  observed        TEXT,
  observed_at     TEXT,
  sealed          TEXT
);

CREATE TABLE operations (
  id               INTEGER PRIMARY KEY,
  run              TEXT NOT NULL,
  op               TEXT NOT NULL,
  queued_at        TEXT NOT NULL,
  started_at       TEXT,
  ended_at         TEXT,
  outcome          TEXT,
  error_code       TEXT,
  cancel_requested INTEGER NOT NULL DEFAULT 0,
  owner_pid        INTEGER NOT NULL,
  owner_boot       TEXT NOT NULL,
  owner_token      TEXT NOT NULL,
  resumed_from     INTEGER REFERENCES operations(id)
);
CREATE INDEX operations_open ON operations(run) WHERE ended_at IS NULL;

CREATE TABLE transitions (
  id         INTEGER PRIMARY KEY,
  run        TEXT NOT NULL REFERENCES runs(name) ON DELETE CASCADE,
  from_state TEXT NOT NULL,
  to_state   TEXT NOT NULL,
  cause      TEXT,
  operation  INTEGER REFERENCES operations(id),
  by         TEXT NOT NULL,
  at         TEXT NOT NULL
);

CREATE TABLE operation_steps (
  operation INTEGER NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
  seq       INTEGER NOT NULL,
  step      TEXT NOT NULL,
  state     TEXT NOT NULL,
  at        TEXT NOT NULL,
  PRIMARY KEY (operation, seq)
);

CREATE TABLE events (
  cursor    INTEGER PRIMARY KEY AUTOINCREMENT,
  run       TEXT,
  operation INTEGER REFERENCES operations(id) ON DELETE SET NULL,
  body      TEXT NOT NULL
);
CREATE INDEX events_run ON events(run, cursor);

CREATE TABLE resources (
  id           INTEGER PRIMARY KEY,
  run          TEXT NOT NULL,
  key          TEXT NOT NULL,
  kind         TEXT NOT NULL,
  scope        TEXT NOT NULL,
  copy_name    TEXT,
  native_id    TEXT,
  state        TEXT NOT NULL CHECK (state IN ('creating', 'created', 'destroying', 'gone')),
  lifetime     TEXT NOT NULL,
  created_by   INTEGER NOT NULL REFERENCES operations(id),
  destroyed_by INTEGER REFERENCES operations(id),
  detail       TEXT,
  updated_at   TEXT NOT NULL
);
CREATE INDEX resources_live ON resources(run) WHERE state <> 'gone';

CREATE TABLE leases (
  scope       TEXT PRIMARY KEY,
  owner_pid   INTEGER NOT NULL,
  owner_boot  TEXT NOT NULL,
  owner_token TEXT NOT NULL,
  acquired_at TEXT NOT NULL,
  expires_at  INTEGER NOT NULL
);

CREATE TABLE port_reservations (
  protocol  TEXT NOT NULL,
  port      INTEGER NOT NULL,
  run       TEXT NOT NULL,
  operation INTEGER REFERENCES operations(id),
  lease     TEXT NOT NULL,
  owner_token TEXT NOT NULL,
  PRIMARY KEY (protocol, port)
);
";
