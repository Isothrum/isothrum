//! The store's schema, as the migrations that build it: each runs once, in order, in the
//! transaction that records it in `schema_migrations`.

/// The migrations, by version from 1. A home whose newest recorded version is past the last is
/// refused (`store_version`): a newer runner wrote it.
pub(super) const MIGRATIONS: &[&str] = &[V1, V2];

/// Version 1: the runs, their transitions, the journal, the events, the resources, the port
/// reservations and the leases.
const V1: &str = r"

CREATE TABLE runs (
  name            TEXT PRIMARY KEY,
  project_path    TEXT NOT NULL,
  source_digest   TEXT NOT NULL,
  format          TEXT NOT NULL,
  desired         TEXT NOT NULL,
  state           TEXT NOT NULL,
  failed_cause    TEXT,
  state_op        INTEGER REFERENCES operations(id),
  state_at        TEXT NOT NULL,
  record          TEXT NOT NULL,
  observed        TEXT,
  observed_at     TEXT
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
  operation INTEGER,
  lease     TEXT NOT NULL,
  owner_token TEXT NOT NULL,
  PRIMARY KEY (protocol, port)
);
";

/// Version 2: the mirror's generation, bumped by every change of the rows the mirror carries
/// (a run but its last observation, an operation, a resource), so the store rewrites the mirror
/// only after a transaction that changed them.
const V2: &str = r"
CREATE TABLE mirror_generation (id INTEGER PRIMARY KEY CHECK (id = 1), n INTEGER NOT NULL);
INSERT INTO mirror_generation (id, n) VALUES (1, 0);
CREATE TRIGGER mirror_runs_insert AFTER INSERT ON runs BEGIN UPDATE mirror_generation SET n = n + 1; END;
CREATE TRIGGER mirror_runs_update AFTER UPDATE OF name, manifest_path, manifest_digest, target, driver, desired, state,
  failed_cause, state_op, state_at, record, sealed ON runs BEGIN UPDATE mirror_generation SET n = n + 1; END;
CREATE TRIGGER mirror_runs_delete AFTER DELETE ON runs BEGIN UPDATE mirror_generation SET n = n + 1; END;
CREATE TRIGGER mirror_operations_insert AFTER INSERT ON operations BEGIN UPDATE mirror_generation SET n = n + 1; END;
CREATE TRIGGER mirror_operations_update AFTER UPDATE ON operations BEGIN UPDATE mirror_generation SET n = n + 1; END;
CREATE TRIGGER mirror_operations_delete AFTER DELETE ON operations BEGIN UPDATE mirror_generation SET n = n + 1; END;
CREATE TRIGGER mirror_resources_insert AFTER INSERT ON resources BEGIN UPDATE mirror_generation SET n = n + 1; END;
CREATE TRIGGER mirror_resources_update AFTER UPDATE ON resources BEGIN UPDATE mirror_generation SET n = n + 1; END;
CREATE TRIGGER mirror_resources_delete AFTER DELETE ON resources BEGIN UPDATE mirror_generation SET n = n + 1; END;
";
