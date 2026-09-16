variable "project_id" {
  description = "Fixed GCP project ID for LouiseLM application infrastructure."
  type        = string
  default     = "louiselm"

  validation {
    condition     = var.project_id == "louiselm"
    error_message = "project_id must remain louiselm because the Firebase and Hosting identity is fixed to that project."
  }
}

variable "state_project_id" {
  description = "Fixed state-only GCP project ID that owns the OpenTofu backend bucket."
  type        = string
  default     = "louiselm-state"

  validation {
    condition     = var.state_project_id == "louiselm-state"
    error_message = "state_project_id must remain louiselm-state because the backend bootstrap boundary is fixed to that project."
  }
}

variable "billing_account_id" {
  description = "Billing account ID explicitly attached to the dedicated project."
  type        = string

  validation {
    condition     = can(regex("^[0-9A-Fa-f]{6}-[0-9A-Fa-f]{6}-[0-9A-Fa-f]{6}$", var.billing_account_id))
    error_message = "billing_account_id must be a GCP billing account ID."
  }
}

variable "org_id" {
  description = "Optional Google Cloud organization ID; org_id and folder_id are mutually exclusive."
  type        = string
  default     = null
  nullable    = true

  validation {
    condition     = var.org_id == null || can(regex("^[0-9]+$", var.org_id))
    error_message = "org_id must be a numeric organization ID."
  }
}

variable "folder_id" {
  description = "Optional Google Cloud folder ID; folder_id and org_id are mutually exclusive."
  type        = string
  default     = null
  nullable    = true

  validation {
    condition     = var.folder_id == null || can(regex("^[0-9]+$", var.folder_id))
    error_message = "folder_id must be a numeric folder ID."
  }
}

variable "android_package_name" {
  description = "Android application package name registered with Firebase."
  type        = string
  default     = "dev.louiselm.capture"

  validation {
    condition     = can(regex("^[A-Za-z][A-Za-z0-9_]*(\\.[A-Za-z][A-Za-z0-9_]*)+$", var.android_package_name))
    error_message = "android_package_name must be a dotted Android package name."
  }
}

variable "android_sha1_fingerprint" {
  description = "SHA-1 fingerprint of the Android signing certificate, without separators."
  type        = string

  validation {
    condition     = can(regex("^[0-9A-Fa-f]{40}$", var.android_sha1_fingerprint))
    error_message = "android_sha1_fingerprint must contain 40 hexadecimal characters."
  }
}

variable "android_sha256_fingerprints" {
  description = "Optional SHA-256 fingerprints of Android signing certificates."
  type        = list(string)
  default     = []

  validation {
    condition     = alltrue([for fingerprint in var.android_sha256_fingerprints : can(regex("^[0-9A-Fa-f]{64}$", fingerprint))])
    error_message = "Every Android SHA-256 fingerprint must contain 64 hexadecimal characters."
  }
}

variable "android_qa_sha1_fingerprint" {
  description = "Optional QA signing SHA-1, without separators; null leaves the separate dev.louiselm.capture.qa app unprovisioned. Retain this input after provisioning."
  type        = string
  default     = null
  nullable    = true

  validation {
    condition     = var.android_qa_sha1_fingerprint == null || can(regex("^[0-9A-Fa-f]{40}$", var.android_qa_sha1_fingerprint))
    error_message = "android_qa_sha1_fingerprint must be null or contain 40 hexadecimal characters."
  }
}

variable "gitlab_issuer_url" {
  description = "Fixed self-managed GitLab OIDC issuer URL."
  type        = string
  default     = "https://gitlab.bartab.fr/"

  validation {
    condition     = var.gitlab_issuer_url == "https://gitlab.bartab.fr/"
    error_message = "gitlab_issuer_url must remain https://gitlab.bartab.fr/ because it is part of the deployment trust boundary."
  }
}

variable "gitlab_project_id" {
  description = "Fixed GitLab project ID allowed to impersonate the Hosting deployer."
  type        = string
  default     = "163"

  validation {
    condition     = var.gitlab_project_id == "163"
    error_message = "gitlab_project_id must remain 163 because it is part of the deployment trust boundary."
  }
}

variable "gitlab_default_branch" {
  description = "Fixed protected GitLab branch allowed to impersonate the Hosting deployer."
  type        = string
  default     = "main"

  validation {
    condition     = var.gitlab_default_branch == "main"
    error_message = "gitlab_default_branch must remain main because it is part of the deployment trust boundary."
  }
}

variable "gitlab_workload_identity_pool_id" {
  description = "Short ID for the GitLab Workload Identity Federation pool."
  type        = string
  default     = "gitlab"

  validation {
    condition     = can(regex("^[a-z0-9-]{4,32}$", var.gitlab_workload_identity_pool_id))
    error_message = "gitlab_workload_identity_pool_id must be 4-32 lowercase letters, digits, or hyphens."
  }
}

variable "gitlab_workload_identity_provider_id" {
  description = "Short ID for the GitLab OIDC provider in the federation pool."
  type        = string
  default     = "louiselm"

  validation {
    condition     = can(regex("^[a-z0-9-]{4,32}$", var.gitlab_workload_identity_provider_id))
    error_message = "gitlab_workload_identity_provider_id must be 4-32 lowercase letters, digits, or hyphens."
  }
}
