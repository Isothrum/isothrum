# isothrum-client

The client of the Isothrum runner service (`isothrum serve`): one service per runner home runs
every operation of that home, one at a time per run, and streams their events. This crate
connects to a home's service (starting it when none answers), sends it typed commands and
follows its event stream as a `Stream`, with replay after a reconnection.

The wire protocol (JSON lines over a Unix domain socket or a Windows named pipe) is in the
`proto` module.
