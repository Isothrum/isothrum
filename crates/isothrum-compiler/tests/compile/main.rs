//! The compiler, through its API: drawings compiled into specs Isoloom accepts, one module per
//! concept (machines, LANs, cables, switches), the fixture drawings as written, and properties
//! over generated drawings; and the docs' tool minimums against what Isoloom generates. `testing`
//! holds what they share.

mod cables;
mod fixtures;
mod lans;
mod machines;
mod properties;
mod switches;
mod testing;
mod tool_versions;
