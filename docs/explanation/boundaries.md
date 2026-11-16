# What Isothrum is, and what it is not

Isothrum is often used with a generator that writes the projects it runs, and under an
application that offers them to its users. This page states the boundary they both rely on.

## What it is

An independent, general-purpose runtime for Compose projects, Vagrantfiles and Terraform modules,
whoever wrote them. It is the runner (`isothrum-runner`), its service (`isothrum-service`) and
client (`isothrum-client`), the `isothrum` command line, and `isothrum-machine`, which reads what
this computer's hypervisors and tools can do.

## What it is not

- **Not tied to a generator.** Isothrum needs no generator. No crate depends on one, nothing
  reads a generator's spec, and nothing assumes a generator's file names, variable names or helper
  services. A generated project runs like any other project of its format.
- **Not an application.** Isothrum knows deployments, machines, services, endpoints and places.
  Labs, CTFs, players and attack boxes are an application's words, and stay in that application.
- **Not one consumer's runtime.** A feature goes in when any Compose, Vagrant or Terraform project
  could use it, under a neutral name. What one application needs on top of that is the
  application's.

## How the boundary is kept

`cargo xtask check` enforces it:

- the `generator-free` step and the `boundary` guardrail: no crate of the workspace, and no
  dependency of one, is a generator's crate;
- the `vocabulary` guardrail: no file under `crates/` (code, tests, fixtures, the crates'
  READMEs) names a generator or uses an application's words (lab, CTF, player, attack box), and
  the documentation names no generator either. The changelog and the migration guides are
  records, and keep the names they had.

## Its boundary with what surrounds it

| Around Isothrum | What it is | How it meets Isothrum |
| --- | --- | --- |
| A generator | A tool that writes Compose, Vagrant or Terraform files from its own spec | Not at all: its files are projects like any other |
| An application | A program that runs projects for its users, under its own words | Through the client, the service and `isothrum-machine`, never by running the tools itself; it converts Isothrum's errors into its own |
