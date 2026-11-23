# The Proxmox VE server self-test's module: one small Debian VM, its first network card on the
# server's bridge (DHCP), its second on a network of the test's own (an SDN simple zone and VNet),
# a cloud-init user that lets in `ssh_public_key`, and the guest agent that reports its address.
# Every value comes from the run (`TF_VAR_<name>`); `selftest::server::proxmox` gives them.

terraform {
  required_version = ">= 1.6.0"
  required_providers {
    proxmox = { source = "bpg/proxmox", version = "~> 0.116.1" }
  }
}

variable "endpoint" {
  type = string
}

variable "api_token" {
  type      = string
  default   = ""
  sensitive = true
}

variable "username" {
  type    = string
  default = "root@pam"
}

variable "password" {
  type      = string
  default   = ""
  sensitive = true
}

variable "insecure" {
  type    = bool
  default = false
}

variable "node" {
  type = string
}

variable "node_ssh_user" {
  type    = string
  default = "root"
}

variable "node_ssh_key_file" {
  type    = string
  default = ""
}

variable "node_address" {
  type    = string
  default = ""
}

variable "datastore" {
  type    = string
  default = "local-lvm"
}

variable "image_datastore" {
  type    = string
  default = "local"
}

variable "snippets_datastore" {
  type    = string
  default = "local"
}

variable "bridge" {
  type    = string
  default = "vmbr0"
}

variable "name" {
  type = string
}

variable "hostname" {
  type = string
}

variable "zone" {
  type = string
}

variable "vnet" {
  type = string
}

variable "user" {
  type    = string
  default = "tester"
}

variable "ssh_public_key" {
  type = string
}

provider "proxmox" {
  endpoint  = var.endpoint
  api_token = var.api_token != "" ? var.api_token : null
  username  = var.api_token != "" ? null : var.username
  password  = var.api_token != "" ? null : var.password
  insecure  = var.insecure
  # The cloud-init snippet is uploaded over SSH to the node.
  ssh {
    agent       = false
    username    = var.node_ssh_user
    password    = var.node_ssh_key_file != "" ? null : var.password
    private_key = var.node_ssh_key_file != "" ? file(var.node_ssh_key_file) : null
    dynamic "node" {
      for_each = var.node_address == "" ? [] : [1]
      content {
        name    = var.node
        address = var.node_address
      }
    }
  }
}

# Applies the SDN deletions on destroy, once the zone and its VNet are gone: every SDN object
# depends on it, so it goes last.
resource "proxmox_sdn_applier" "selftest_finalizer" {}

# After the image: the test's steps end in their order (the image, then the network, then the VM).
resource "proxmox_sdn_zone_simple" "selftest_zone" {
  id         = var.zone
  nodes      = [var.node]
  depends_on = [proxmox_sdn_applier.selftest_finalizer, proxmox_download_file.selftest_image]
}

resource "proxmox_sdn_vnet" "selftest_vnet" {
  id   = var.vnet
  zone = proxmox_sdn_zone_simple.selftest_zone.id
}

# Applies the zone and its VNet once created.
resource "proxmox_sdn_applier" "selftest_net" {
  lifecycle {
    replace_triggered_by = [proxmox_sdn_zone_simple.selftest_zone, proxmox_sdn_vnet.selftest_vnet]
  }
  depends_on = [proxmox_sdn_vnet.selftest_vnet]
}

resource "proxmox_download_file" "selftest_image" {
  node_name           = var.node
  datastore_id        = var.image_datastore
  content_type        = "iso"
  url                 = "https://cloud.debian.org/images/cloud/bookworm/20261015-2632/debian-12-genericcloud-amd64-20261015-2632.qcow2"
  file_name           = "${var.name}-debian-12.img"
  overwrite_unmanaged = true
  checksum            = "56c0bd55b6623f2739f377e983d2872e7b208d0cb9c3ba4e93d3f72929026da03637c598b3bd472b29998c31c8a4c17d8e6ae1c556852a8b83a66a09bf5c2b37"
  checksum_algorithm  = "sha512"
}

resource "proxmox_virtual_environment_file" "selftest_user_data" {
  node_name    = var.node
  datastore_id = var.snippets_datastore
  content_type = "snippets"
  source_raw {
    file_name = "${var.name}.yaml"
    data = format("#cloud-config\n%s", yamlencode({
      hostname       = var.hostname
      users          = [{ name = var.user, sudo = "ALL=(ALL) NOPASSWD:ALL", shell = "/bin/bash", ssh_authorized_keys = [var.ssh_public_key] }]
      package_update = true
      packages       = ["qemu-guest-agent"]
      runcmd         = ["systemctl enable --now qemu-guest-agent"]
    }))
  }
}

# The guest agent reports the VM's address on the bridge (DHCP): its creation waits for it.
resource "proxmox_virtual_environment_vm" "selftest_vm" {
  name            = var.name
  node_name       = var.node
  tags            = ["selftest"]
  on_boot         = false
  stop_on_destroy = true
  agent {
    enabled = true
    timeout = "8m"
    wait_for_ip {
      ipv4 = true
    }
  }
  lifecycle {
    postcondition {
      condition     = length([for a in try(flatten(self.ipv4_addresses), []) : a if a != "127.0.0.1"]) > 0
      error_message = "the VM got no IPv4 address on bridge `${var.bridge}` within 8m: the bridge has no DHCP, or no DNS or internet to install qemu-guest-agent, which reports the address."
    }
  }
  cpu {
    cores = 1
    type  = "host"
  }
  memory {
    dedicated = 512
  }
  disk {
    datastore_id = var.datastore
    file_id      = proxmox_download_file.selftest_image.id
    interface    = "virtio0"
    size         = 8
  }
  network_device {
    bridge = var.bridge
  }
  network_device {
    bridge = proxmox_sdn_vnet.selftest_vnet.id
  }
  initialization {
    datastore_id      = var.datastore
    user_data_file_id = proxmox_virtual_environment_file.selftest_user_data.id
    ip_config {
      ipv4 {
        address = "dhcp"
      }
    }
  }
  operating_system {
    type = "l26"
  }
  serial_device {}
  depends_on = [proxmox_sdn_applier.selftest_net]
}

output "ssh_user" {
  value = var.user
}
