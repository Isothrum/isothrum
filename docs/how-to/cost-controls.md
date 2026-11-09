# Control costs

A cloud run bills until it is destroyed. Isothrum gives each cloud run an expiry and can sweep
the expired ones; you schedule the sweep.

## Give a run a lifetime

```sh
isothrum up --lifetime 120     # minutes
```

For a manifest with an `expiry` (Isoloom's cloud targets have one), `up` sets the run's expiry to
now plus the lifetime: `--lifetime`, else the manifest's default (`expiry.default_minutes`, 240 in
Isoloom's cloud targets), else 240 minutes. Isothrum records it in the run's record
(`expires_at`) and passes it to the Terraform module as the variable the manifest names, in Unix
seconds. Isoloom's cloud modules tag what they create with it; what else a module does with it
is the module's.

The lifetime is set when the run starts. A manifest without an expiry (Compose, Vagrant, Proxmox)
ignores `--lifetime`.

## Sweep expired runs

```sh
isothrum reap aws --region eu-west-3 \
  --secret-from-env AWS_ACCESS_KEY_ID --secret-from-env AWS_SECRET_ACCESS_KEY
```

`reap <cloud>` (`aws`, `azure`, `gcp`, `digitalocean`, `linode`, `oci`):

1. **destroys** every run of this home on that cloud whose expiry has passed and that isn't
   already `absent`: each one's `down`, with the params it was started with and the credentials
   given here;
2. **lists** what the cloud still holds tagged past its expiry that no recorded run covers. These
   are never deleted: nothing says what they belong to, so a person removes them.

It exits 1 when a destroy failed, when something is left, or when the cloud couldn't be queried
(so a sweep that couldn't look never passes for a clean one). `--dry-run` destroys nothing and
says what it would destroy. `--region` (AWS, repeatable) and `--project` (Google Cloud) say where to
look for leftovers; the default is the cloud command line's own.

The destroy reopens each run's manifest at the path its record holds, so the run's generated files
must still be there, and the home must be the one the runs were started with.

## Schedule it

Run `reap` where the home lives, every few minutes or hours. With cron:

```cron
*/30 * * * * ISOTHRUM_HOME=/srv/isothrum /usr/local/bin/isothrum reap aws --region eu-west-3 --secret-from-env AWS_ACCESS_KEY_ID --secret-from-env AWS_SECRET_ACCESS_KEY >> /var/log/isothrum-reap.log 2>&1
```

Cron's environment must carry the credentials the `--secret-from-env` names (a wrapper script
that loads them, for instance). A non-zero exit is your alert.

A scheduled CI job that starts with an empty home has no records: it destroys nothing and only
lists the leftovers, which still fails the job when there are any. That is still worth running as
an alarm for runs whose home was lost.

## Stop paying without destroying

`isothrum park` stops a run and keeps what it created (`--mode shutdown`, the default, or
`--mode pause`); `isothrum resume` (or `up`) starts it again. A target that can't park refuses with
`not_parkable`. What a parked run still costs (disks, addresses) is the cloud's billing.
