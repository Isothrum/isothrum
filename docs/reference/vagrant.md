# Vagrant projects

Isothrum runs any Vagrant project: a folder that holds a `Vagrantfile`. It needs nothing of its
own in it. The run's values and secrets reach the Vagrantfile as environment variables, which it
reads with `ENV.fetch`. The file is never rewritten. The convention below is optional: a
Vagrantfile that follows none of it still runs.

## Stable host ports

A forwarded port whose host port is taken at a boot is moved by Vagrant's `auto_correct` to a free
one (`8080` to `2200`). Most providers pick again at every later boot, so a resume after a shutdown
can move the run's address back (`2200` to `8080`):

| Provider | At a later boot |
|---|---|
| VirtualBox | Keeps the port it moved: it reads the VM's own forwards back. |
| vagrant-qemu | Picks again: its collision step keeps no earlier ports. |
| UTM (vagrant_utm) | Clears the VM's forwards and picks again at every boot. |
| libvirt, VMware, Hyper-V | Not checked yet. |

No provider takes a forwarded port's host side from outside the Vagrantfile. Isothrum keeps the
host port each machine published at the run's first start, until `down`, and before every later
operation it sets one variable per kept port:

```
ISOTHRUM_HOST_PORT_<MACHINE>_<GUEST>_<PROTO>=<host port>
```

- `<MACHINE>`: the machine's name upper-cased. Every character other than `A`–`Z` and `0`–`9` is
  written `_`, one for each (a hyphen, a dot, a non-ASCII character: `web-1.lab` is `WEB_1_LAB`,
  `web·1` is `WEB_1`).
- `<GUEST>`: the guest port, in decimal (`80`).
- `<PROTO>`: `TCP` or `UDP`. A forward whose protocol Vagrant doesn't say is `TCP`.
- When two machines' names give the same `<MACHINE>` (`web-1` and `web_1`), neither machine gets
  that port's variable.

A Vagrantfile that wants stable ports reads it, with its own port as the fallback, so it runs the
same without Isothrum:

```ruby
config.vm.define "web" do |web|
  web.vm.network "forwarded_port", guest: 80,
    host: ENV.fetch("ISOTHRUM_HOST_PORT_WEB_80_TCP", "8080").to_i, auto_correct: true
end
```

When a machine's host port still moves (a Vagrantfile that doesn't read the variable, or a port
taken meanwhile), the operation warns `ports_moved`, naming the machine, the port and both host
ports. The run keeps the new one from then on.
