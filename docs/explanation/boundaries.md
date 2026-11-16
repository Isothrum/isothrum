# What Isothrum is, and what it is not

Isothrum is one of three products that are often used together. Each has a boundary the others
rely on. This page states Isothrum's.

## What it is

An independent, general-purpose runtime for Compose projects, Vagrantfiles and Terraform modules,
whoever wrote them. It is the runner (`isothrum-runner`), its service (`isothrum-service`) and
client (`isothrum-client`), the `isothrum` command line, and `isothrum-machine`, which reads what
this computer's hypervisors and tools can do.

## What it is not

- **Not tied to a generator.** Isothrum doesn't need [Isoloom](https://github.com/isoloom/isoloom)
  or any other generator. No crate depends on an Isoloom crate, nothing reads an Isoloom spec, and
  nothing assumes Isoloom's file names, variable names or helper services. A project Isoloom
  generated runs like any other project of its format.
- **Not an application.** Isothrum knows deployments, machines, services, endpoints and places.
  Labs, CTFs, players and attack boxes are an application's words, and stay in that application.
- **Not one consumer's runtime.** A feature goes in when any Compose, Vagrant or Terraform project
  could use it, under a neutral name. What one application needs on top of that is the
  application's.

## How the boundary is kept

`cargo xtask check` enforces it:

- the `isoloom-free` step and the `boundary` guardrail: no crate of the workspace, and no
  dependency of one, is an Isoloom crate.

## Its boundary with the other products

| Product | What it is | How it meets Isothrum |
| --- | --- | --- |
| [Isoloom](https://github.com/isoloom/isoloom) | A converter: one spec into each target's native files | Not at all: its files are projects like any other |
| [Cyber CTF launcher](https://github.com/Cyber-Courses/Cyber-CTF-Launcher) | A desktop app that runs capture-the-flag labs | A thin layer over Isothrum's client, service and `isothrum-machine`: it runs no tool itself, and converts Isothrum's errors into its own in one place |
