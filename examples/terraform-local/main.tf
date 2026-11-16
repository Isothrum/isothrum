# A Terraform module that needs no cloud and no download: `terraform_data` is Terraform's own
# resource, so `init` fetches nothing and `apply` creates nothing outside the state.

terraform {
  required_version = ">= 1.6.0"
}

variable "greeting" {
  type        = string
  default     = "hello"
  description = "What the message says first. `isothrum up --env greeting=hi` changes it."
}

# A value kept in the state and replaced when the greeting changes.
resource "terraform_data" "message" {
  input = "${var.greeting} from a Terraform module"
}

# A second resource that follows the first, as a resource built on another would.
resource "terraform_data" "stamp" {
  triggers_replace = [terraform_data.message.output]
  input            = length(terraform_data.message.output)
}

output "message" {
  value = terraform_data.message.output
}

output "length" {
  value = terraform_data.stamp.output
}
