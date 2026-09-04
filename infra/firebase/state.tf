locals {
  state_required_services = toset([
    "cloudresourcemanager.googleapis.com",
    "policytroubleshooter.googleapis.com",
    "serviceusage.googleapis.com",
    "storage.googleapis.com",
  ])
}

resource "google_project" "state" {
  project_id          = var.state_project_id
  name                = "LouiseLM State"
  billing_account     = var.billing_account_id
  org_id              = local.project_parent_valid ? var.org_id : null
  folder_id           = local.project_parent_valid ? var.folder_id : null
  auto_create_network = false
  deletion_policy     = "PREVENT"

  lifecycle {
    prevent_destroy = true

    precondition {
      condition     = local.project_parent_valid
      error_message = "At most one of org_id or folder_id may be supplied to create the state project."
    }
  }
}

resource "google_project_service" "state_required" {
  for_each = local.state_required_services

  project            = google_project.state.project_id
  service            = each.value
  disable_on_destroy = false
  deletion_policy    = "PREVENT"
}

resource "google_project_iam_audit_config" "state" {
  project = google_project.state.project_id
  service = "allServices"

  depends_on = [google_project_service.state_required]

  lifecycle {
    prevent_destroy = true
  }

  audit_log_config {
    log_type = "ADMIN_READ"
  }

  audit_log_config {
    log_type = "DATA_READ"
  }

  audit_log_config {
    log_type = "DATA_WRITE"
  }
}

# Google-managed encryption avoids making state recovery depend on a KMS key
# in the project whose state is being recovered. Data Access audit events are
# supplemental evidence; a second legacy usage-log bucket adds a bootstrap edge.
#trivy:ignore:GCP-0066
#trivy:ignore:GCP-0077
resource "google_storage_bucket" "state" {
  name                        = "louiselm-tfstate"
  project                     = google_project.state.project_id
  location                    = "EU"
  storage_class               = "STANDARD"
  uniform_bucket_level_access = true
  public_access_prevention    = "enforced"
  force_destroy               = false
  deletion_policy             = "PREVENT"

  versioning {
    enabled = true
  }

  soft_delete_policy {
    retention_duration_seconds = 604800
  }

  lifecycle {
    prevent_destroy = true
  }

  depends_on = [google_project_service.state_required["storage.googleapis.com"]]
}

resource "google_resource_manager_lien" "state" {
  parent          = "projects/${google_project.state.project_id}"
  origin          = "louiselm-opentofu-state"
  reason          = "Protect the isolated LouiseLM OpenTofu state project."
  restrictions    = ["resourcemanager.projects.delete"]
  deletion_policy = "PREVENT"

  lifecycle {
    prevent_destroy = true
  }

  depends_on = [google_project_service.state_required["cloudresourcemanager.googleapis.com"]]
}

resource "google_resource_manager_lien" "notifications" {
  parent          = "projects/${google_project.notifications.project_id}"
  origin          = "louiselm-opentofu"
  reason          = "Protect the LouiseLM Firebase and Hosting project."
  restrictions    = ["resourcemanager.projects.delete"]
  deletion_policy = "PREVENT"

  lifecycle {
    prevent_destroy = true
  }

  depends_on = [google_project_service.required["cloudresourcemanager.googleapis.com"]]
}
