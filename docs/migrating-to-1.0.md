# Migrating to 1.0

Isothrum 1.0 pairs with Isoloom 1.0. There is no backward compatibility: the runner reads
the 1.0 run manifest only, and the public API changed. The full spec-format migration
lives in Isoloom's
[`docs/migrating-to-1.0.md`](https://github.com/isoloom/isoloom/blob/main/docs/migrating-to-1.0.md);
this note covers what changes for Isothrum's own consumers.

## The run API lost the "lab" vocabulary

Isothrum is a general-purpose runner, so the word "lab" is gone from the
public API:

| 0.x | 1.0 |
|---|---|
| `Lab<'r>` (from `Runner::open`) | `Running<'r>` |
| `status::Run` (the up/resume result) | `Started` |
| compiler `LabFile` | `ProjectFile` |
| CLI `LabArgs` | `TargetArgs` |
| `LabArgs.lab` (its path field) | `TargetArgs.path` |
| the `lab` module | `run` |

The public runner types are now `Runner` (`open` returns `Running<'r>`), `Running<'r>` and
`Started`.

## The manifest key `lab:` is now `environment:`

The runner consumes the run manifest's top-level provenance under `environment:` (was
`lab:`). The shape is identical. The manifest is validated strictly, so a manifest still
carrying `lab:` is rejected.

## No change to the CLI operations

`up` (with `--replace` / `--lifetime`), `park`, `resume`, `status`, `check`, `provision`,
`down` and `reap` are unchanged in shape. They run against the manifest Isoloom writes at
`<run>/.isoloom/<target>/run.json`.
