# A Terraform module that needs no cloud

[`main.tf`](main.tf) is an ordinary Terraform root module. Its two resources are `terraform_data`,
Terraform's own resource type: `init` downloads no provider, and `apply` creates nothing outside
Terraform's state. It takes one variable, `greeting`, and gives two outputs, `message` and
`length`.

Needs Terraform 1.6.0 or later (`terraform version`).

## Bring it up

```sh
cd examples/terraform-local
isothrum up
```

Isothrum runs `terraform init`, then `terraform apply`. Terraform's own lines come first, then the
outputs:

```text
The run is up.
length: 29
message: hello from a Terraform module
```

The run is named after the folder, `terraform-local`. Its Terraform state is the run's own, under
`~/.isothrum/runs/terraform-local/`: nothing is written into this folder.

## Read its status

```sh
isothrum status
```

```text
terraform-local: ready
length: 29
message: hello from a Terraform module
```

## Give it a value

`--env NAME=VALUE` reaches the module's variable `NAME`. `update` applies the module again with it:

```sh
isothrum update --env greeting=hi
isothrum status
```

```text
terraform-local: ready
length: 26
message: hi from a Terraform module
```

The value is recorded: later commands give it again.

## Commands, park and resume

`exec` runs a command in one of a module's virtual machines, over SSH. This module makes none, so
there is no machine to run it in:

```sh
isothrum exec vm -- uname -a
```

```text
error: run `terraform-local` has no machine `vm`
```

A module has no stopped state either: its resources are made by `apply` and removed by `destroy`
only. `park` and `resume` are refused with the error code `unsupported`:

```sh
isothrum park
```

```text
error: parking on a Terraform run: a module's machines are made and removed by its apply and destroy only isn't supported
```

A module whose resources are VMs (on Proxmox VE, a cloud, ESXi) gets `exec` and `console` into
each one: [Terraform modules](../../docs/reference/terraform.md).

## Clean up

```sh
isothrum down
isothrum status
```

```text
terraform-local: absent
```

`down` runs `terraform destroy` with the run's state and values.
