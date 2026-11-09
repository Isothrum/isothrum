# Manage the SSH key

The runner has **one SSH key per home**: `<home>/ssh/id_ed25519` and `id_ed25519.pub`. It makes
the key the first time a run needs SSH (`ssh-keygen -t ed25519`, no passphrase, comment
`isothrum`), or when you ask for it:

```sh
isothrum key        # prints the public key, making the pair if there is none
```

## Where it is used

Every SSH connection the runner makes uses this key and no other: checks and ready conditions run
over SSH, secrets delivered into a machine (`guest`), and the consoles of machines Terraform made.
Generated cloud modules receive the public key as a variable and install it on the machines they
create. Your own `~/.ssh/config` and agent keys are not used: each run gets its own
`ssh_config` with `IdentitiesOnly yes`.

When a run reaches its machines through a jump host (`--ssh-jump user@host` at `up`, recorded
for the run's later operations), authorize the key there:

```sh
isothrum key | ssh user@jump-host 'cat >> ~/.ssh/authorized_keys'
```

## Host keys

Each run has its own `known_hosts` in its state folder. A machine's host key is accepted the first
time and pinned for the run's life (`StrictHostKeyChecking accept-new`); a changed key afterwards
fails the connection.

## Several homes, several keys

Each home has its own key, so two homes (two CI pipelines, two users) never share one. To use one
key for several homes, copy `ssh/` from one home to the other before its first use, keeping the
folder `0700` and the private key `0600`.

## Use your own key

Put the pair at `<home>/ssh/id_ed25519` and `<home>/ssh/id_ed25519.pub` before the first
operation that needs SSH. The runner connects in batch mode, so the key must have no passphrase.

## Rotate the key

The machines of a live run accept only the key they were given. Rotate when no run that uses SSH
is up (or after `down`):

```sh
rm -r "$ISOTHRUM_HOME/ssh"
isothrum key        # the new public key; authorize it on your jump hosts again
```

## Protect it

The private key has no passphrase and gives access to every live run's machines and to the jump
hosts that authorize it. It is as sensitive as the home itself: keep the home private to the
account that runs Isothrum, out of shared folders and backups others can read, and treat a CI
artifact that carries the home as a secret.
