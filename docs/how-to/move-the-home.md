# Move the home

The runner's home holds every run's record, lock, events and state (Terraform state included),
the runner's SSH key and the Terraform plugin cache. It is `--home DIR`, else `$ISOTHRUM_HOME`,
else `~/.isothrum`. See [State layout](../reference/state.md).

## Move it

1. Make sure no `isothrum` command is running (nothing holds a run's lock).
2. Move the folder, keeping its permissions:

   ```sh
   mv ~/.isothrum /data/isothrum
   ```

   Across file systems, `mv` copies; `cp -a` or `tar` keep the modes too. The SSH key must stay
   readable by you only, or `ssh` refuses it.
3. Point every later command at the new place:

   ```sh
   export ISOTHRUM_HOME=/data/isothrum     # in your shell profile, service or CI settings
   isothrum status <run folder>
   ```

   On Windows: `setx ISOTHRUM_HOME D:\isothrum`, then open a new terminal. Keep the home inside
   your user profile, or give the new folder the same restricted permissions.

## What keeps working

- **Compose runs**: their state is the pinned ports, read from the run's folder in the home.
- **Terraform runs**: the state and `TF_DATA_DIR` are under the run's folder in the home, and every
  operation passes their paths again (`down` runs `terraform init` before `destroy`).
- **The SSH key** moves with the home, so machines that authorized it still accept it.

The record holds the path of the run's manifest (in the run's folder, outside the home), which a
move of the home doesn't change. Moving a run's folder is a different matter: commands given the
new folder work, but `isothrum reap` reopens the path recorded at the last `up` or `update`.

## Vagrant runs

Vagrant's own state for a run is in the run's folder in the home (`VAGRANT_DOTFILE_PATH`), and
Vagrant also keeps a global index of its machines by path. Moving a home with a Vagrant run still
up hasn't been tested: bring Vagrant runs down (or move the home while none exists) to be safe.

## Don't copy it to work in parallel

Two copies of a home both believe they own the same runs. Operating a run from both can destroy
or recreate resources the other still records, and the lock only guards one home. Copy a home only
as a backup, and restore it in place of the original.
