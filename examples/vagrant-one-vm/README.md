# A Vagrant project: one virtual machine

[`Vagrantfile`](Vagrantfile) is an ordinary Vagrantfile with one machine, `server`: a Debian VM
that a shell provisioner sets up. `vagrant up` runs it as it is; Isothrum runs it too, with nothing
added to the folder.

Needs Vagrant 2.4.0 or later (`vagrant --version`) and a hypervisor Vagrant drives. The box,
`bento/debian-12`, is built for VirtualBox, VMware and Parallels; on another provider give a box
built for it with `--env BOX=<box>`.

## Bring it up

```sh
cd examples/vagrant-one-vm
isothrum up --provider virtualbox
```

`--provider` names the hypervisor (`virtualbox`, `vmware_desktop`, `hyperv`, `parallels`,
`libvirt`, `qemu`, `utm`); the run keeps it, so later commands don't repeat it. Isothrum checks
Vagrant and the provider, then runs `vagrant up`, which downloads the box the first time, boots the
VM and runs the provisioner. Vagrant's own lines come first, then:

```text
The run is up.
```

The run is named after the folder, `vagrant-one-vm`. Vagrant's record of the VM (its `.vagrant`
folder) is kept under `~/.isothrum/runs/vagrant-one-vm/`: nothing is written into this folder.

## Read its status

```sh
isothrum status
```

```text
vagrant-one-vm: ready
  server                   running      2222->22
```

Vagrant forwards the VM's SSH port, 22, to a port of this computer; yours may differ.

## Run a command in the VM

```sh
isothrum exec server -- cat /etc/greeting
```

```text
set up by Vagrant's shell provisioner
```

`exec` runs the command over Vagrant's SSH, prints what it prints and exits with its exit status.

## Park it and resume it

```sh
isothrum park
isothrum status
```

```text
vagrant-one-vm: parked
  server                   stopped
```

`park` shuts the VM down (`vagrant halt`) and keeps its disk. `park --mode pause` suspends it
instead, keeping its memory too.

```sh
isothrum resume
```

```text
The run is up again.
```

`resume` boots it again without running the provisioner a second time.

## Clean up

```sh
isothrum down
isothrum status
```

```text
vagrant-one-vm: absent
  server                   absent
```

`down` destroys the VM and its disk (`vagrant destroy -f`).
