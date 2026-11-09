# Run offline or air-gapped

Isothrum needs no network access of its own: it reads local files and runs local tools. What
needs the internet is what the tools download. Prepare those on a connected machine, carry them
in, and point the tools at them.

## Container images (Compose targets)

List every image the generated Compose file uses, pull them on a connected machine, and load them
on the closed one (or push them to an internal registry the engine can reach):

```sh
docker compose -f .isoloom/docker/compose.yml config --images > images.txt
xargs -n1 docker pull < images.txt
docker save $(cat images.txt) -o images.tar     # carry images.tar across
docker load -i images.tar                       # on the closed machine
```

Services in a Compose profile (an on-demand access machine) are listed with `--profile <name>`.
Images the Compose file builds (`build:`) need their base images, and whatever their build
downloads, available too.

On macOS, the Proxmox targets run Terraform from the image `hashicorp/terraform:1.9.8` pinned by
digest (see the manifest's `tools`): load it the same way.

## Vagrant boxes and plugins

Add each box the Vagrantfile names from a file, and install the providers' plugins from their gem
files, before the first `up`:

```sh
vagrant box add --name <box> /media/boxes/<box>.box
vagrant plugin install /media/plugins/<plugin>.gem
```

## Terraform providers

Terraform downloads the providers each module requires at `terraform init`. Give it a
[provider mirror](https://developer.hashicorp.com/terraform/cli/config/config-file#provider-installation)
through a CLI configuration file named by `TF_CLI_CONFIG_FILE`:

1. On a connected machine, in each generated module's folder, fill a mirror, then copy it to the
   closed machine (here to `/opt/terraform-providers`):

   ```sh
   terraform providers mirror /media/terraform-providers
   ```

2. On the closed machine, write a CLI configuration that installs from it only:

   ```hcl
   # /etc/terraform/offline.tfrc
   provider_installation {
     filesystem_mirror {
       path    = "/opt/terraform-providers"
       include = ["registry.terraform.io/*/*"]
     }
     direct {
       exclude = ["registry.terraform.io/*/*"]
     }
   }
   ```

3. Run Isothrum with it:

   ```sh
   export TF_CLI_CONFIG_FILE=/etc/terraform/offline.tfrc
   isothrum up
   ```

`TF_CLI_CONFIG_FILE` reaches the host's Terraform (Isothrum withholds only the cloud settings,
`TF_VAR_*` and `TF_CLI_ARGS*`). Unless you set `TF_PLUGIN_CACHE_DIR`, Isothrum sets it to
`<home>/terraform-plugins`, so a provider installed once is reused by every run of the home.

When Terraform runs from its container image (Proxmox targets on macOS), the container gets
`TF_CLI_CONFIG_FILE` and the file it names (read-only), but not the mirror's folder: there, use a
[`network_mirror`](https://developer.hashicorp.com/terraform/cli/config/config-file#network_mirror)
on an internal server instead of a `filesystem_mirror`. See
[Environment variables](../reference/environment.md).

## Check it

Cut the network and run `isothrum up` once: a missing image, box or provider fails the step that
needed it, with the tool's message in the step's `log` events.
