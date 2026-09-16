terraform {
  required_version = ">= 1.12.0, < 2.0.0"
}

variable "config_file_contents" {
  description = "Base64 SDK configuration returned by Firebase."
  type        = string
  sensitive   = true
}

variable "package_name" {
  description = "Android package whose SDK client is being exported."
  type        = string
}

variable "api_key" {
  description = "Existing API key restricted to the exported Android package and signer."
  type        = string
  sensitive   = true
}

locals {
  sdk_config = jsondecode(base64decode(var.config_file_contents))
  # Firebase may return other clients and multiple keys in arbitrary order.
  clients = [for client in local.sdk_config.client : merge(client, {
    api_key = [{ current_key = var.api_key }]
  }) if client.client_info.android_client_info.package_name == var.package_name]
}

output "config_json" {
  description = "Android SDK configuration for the requested package and key."
  value       = jsonencode(merge(local.sdk_config, { client = local.clients }))
  sensitive   = true

  precondition {
    condition     = length(local.clients) == 1
    error_message = "Firebase SDK configuration must contain exactly one client matching the requested Android package."
  }
}
