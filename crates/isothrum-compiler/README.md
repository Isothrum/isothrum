# isothrum-compiler

Turns a network topology drawn in an editor (machines, the LANs drawn around them, the cables
between their ports) into an [Isoloom](https://github.com/isoloom/isoloom) spec (`isoloom.yml`,
version 1), plus the project files the spec refers to (appliance configurations, a starter for
each provisioning step). Every spec it returns has been read back by Isoloom's own parser.

The topology is JSON (`Topology`, serde). Whether the project is valid as a whole is left to
Isoloom's validation.

## Install

```sh
cargo add isothrum-compiler
cargo add serde_json  # to read the topology
```

## Example

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let topology: isothrum_compiler::Topology = serde_json::from_str(
        r#"{
          "name": "pair",
          "networks": { "office": { "cidr": "10.10.1.0/24" } },
          "nodes": {
            "web":  { "kind": { "container": { "image": "nginx:1.27-alpine" } },
                      "services": [{ "port": 80, "protocol": "http" }], "networks": { "office": null } },
            "user": { "kind": { "vm": { "os": "debian@12" } }, "access": true, "networks": { "office": null } }
          }
        }"#,
    )?;
    let compiled = isothrum_compiler::compile(&topology)?;
    // The spec's text, the spec as Isoloom parsed it, and the project's own files.
    let (yaml, _spec, files) = compiled.into_parts();
    std::fs::write("isoloom.yml", yaml)?;
    println!("{} project files", files.len());
    Ok(())
}
```

## Feature flags

None.

## Documentation

- API: <https://docs.rs/isothrum-compiler>
- Guides and reference: <https://github.com/Isothrum/isothrum/blob/main/docs/README.md>

Minimum Rust: 1.90. Licence: Apache-2.0.
