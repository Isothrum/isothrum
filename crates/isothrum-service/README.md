# isothrum-service

The Isothrum runner service (`isothrum serve --home <home>`): one daemon per runner home. It
holds the home (one service per home), queues each run's operations (one at a time per run, runs
side by side), runs them, and streams their events to its clients over an owner-only local
endpoint (a Unix domain socket, a Windows named pipe). Clients use `isothrum-client`.
