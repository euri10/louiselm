locals {
  project_parent_valid = var.org_id == null || var.folder_id == null

  required_services = toset([
    "apikeys.googleapis.com",
    "cloudbilling.googleapis.com",
    "cloudresourcemanager.googleapis.com",
    "compute.googleapis.com",
    "fcm.googleapis.com",
    "fcmregistrations.googleapis.com",
    "firebase.googleapis.com",
    "firebasehosting.googleapis.com",
    "firebaseinstallations.googleapis.com",
    "iam.googleapis.com",
    "iamcredentials.googleapis.com",
    "logging.googleapis.com",
    "serviceusage.googleapis.com",
    "sts.googleapis.com",
  ])
}

resource "google_project" "notifications" {
  project_id          = var.project_id
  name                = "LouiseLM"
  billing_account     = var.billing_account_id
  org_id              = local.project_parent_valid ? var.org_id : null
  folder_id           = local.project_parent_valid ? var.folder_id : null
  auto_create_network = false
  deletion_policy     = "PREVENT"

  lifecycle {
    prevent_destroy = true

    precondition {
      condition     = local.project_parent_valid
      error_message = "At most one of org_id or folder_id may be supplied to create the dedicated project."
    }
  }
}

resource "google_project_iam_audit_config" "notifications" {
  project = google_project.notifications.project_id
  service = "allServices"

  depends_on = [google_firebase_project.notifications]

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

resource "google_project_service" "required" {
  for_each = local.required_services

  project            = google_project.notifications.project_id
  service            = each.value
  disable_on_destroy = false
  deletion_policy    = "PREVENT"
}

resource "google_project_default_service_accounts" "app" {
  project        = google_project.notifications.project_id
  action         = "DEPRIVILEGE"
  restore_policy = "NONE"

  depends_on = [google_project_service.required]
}

resource "google_firebase_project" "notifications" {
  provider = google-beta
  project  = google_project.notifications.project_id

  lifecycle {
    prevent_destroy = true
  }

  depends_on = [google_project_default_service_accounts.app]
}

resource "google_firebase_hosting_site" "public" {
  provider = google-beta

  project         = google_project.notifications.project_id
  site_id         = google_project.notifications.project_id
  deletion_policy = "PREVENT"

  lifecycle {
    prevent_destroy = true
  }

  depends_on = [
    google_firebase_project.notifications,
    google_project_service.required,
  ]
}

resource "google_apikeys_key" "android" {
  provider = google-beta

  name            = "louiselm-android"
  display_name    = "LouiseLM Android Firebase key"
  project         = google_project.notifications.project_id
  deletion_policy = "PREVENT"

  restrictions {
    android_key_restrictions {
      allowed_applications {
        package_name     = var.android_package_name
        sha1_fingerprint = lower(var.android_sha1_fingerprint)
      }
    }

    api_targets {
      service = "firebase.googleapis.com"
    }

    api_targets {
      service = "fcmregistrations.googleapis.com"
    }

    api_targets {
      service = "firebaseinstallations.googleapis.com"
    }

    api_targets {
      service = "logging.googleapis.com"
    }
  }

  depends_on = [google_firebase_project.notifications]

  lifecycle {
    prevent_destroy = true
  }
}

resource "google_firebase_android_app" "notifications" {
  provider = google-beta

  project         = google_project.notifications.project_id
  display_name    = "LouiseLM Android"
  package_name    = var.android_package_name
  sha1_hashes     = [lower(var.android_sha1_fingerprint)]
  sha256_hashes   = [for fingerprint in var.android_sha256_fingerprints : lower(fingerprint)]
  api_key_id      = google_apikeys_key.android.uid
  deletion_policy = "PREVENT"

  depends_on = [google_firebase_project.notifications]
}

data "google_firebase_android_app_config" "notifications" {
  provider = google-beta

  project = google_project.notifications.project_id
  app_id  = google_firebase_android_app.notifications.app_id
}

resource "google_service_account" "sender" {
  project         = google_project.notifications.project_id
  account_id      = "louiselm-fcm-sender"
  display_name    = "LouiseLM FCM sender"
  deletion_policy = "PREVENT"

  depends_on = [google_project_service.required]

  lifecycle {
    prevent_destroy = true
  }
}

resource "google_project_iam_custom_role" "sender" {
  project         = google_project.notifications.project_id
  role_id         = "louiselmFcmSender"
  title           = "LouiseLM FCM sender"
  description     = "Only send Firebase Cloud Messaging messages for LouiseLM."
  permissions     = ["cloudmessaging.messages.create"]
  stage           = "GA"
  deletion_policy = "PREVENT"

  depends_on = [google_project_service.required]

  lifecycle {
    prevent_destroy = true
  }
}

resource "google_project_iam_member" "sender" {
  project = google_project.notifications.project_id
  role    = google_project_iam_custom_role.sender.name
  member  = "serviceAccount:${google_service_account.sender.email}"

  depends_on = [google_firebase_project.notifications]
}

resource "google_iam_workload_identity_pool" "gitlab" {
  project                   = google_project.notifications.project_id
  workload_identity_pool_id = var.gitlab_workload_identity_pool_id
  display_name              = "LouiseLM GitLab CI"
  description               = "Keyless protected-main deployment access for the LouiseLM site."
  disabled                  = false
  deletion_policy           = "PREVENT"

  depends_on = [google_project_service.required]

  lifecycle {
    prevent_destroy = true
  }
}

resource "google_iam_workload_identity_pool_provider" "gitlab" {
  project                            = google_project.notifications.project_id
  workload_identity_pool_id          = google_iam_workload_identity_pool.gitlab.workload_identity_pool_id
  workload_identity_pool_provider_id = var.gitlab_workload_identity_provider_id
  display_name                       = "LouiseLM self-managed GitLab"
  deletion_policy                    = "PREVENT"
  attribute_condition                = "assertion.project_id == '${var.gitlab_project_id}' && assertion.ref_type == 'branch' && assertion.ref == '${var.gitlab_default_branch}' && assertion.ref_protected == 'true' && assertion.environment == 'site-production'"
  attribute_mapping = {
    "google.subject"          = "assertion.sub"
    "attribute.environment"   = "assertion.environment"
    "attribute.project_id"    = "assertion.project_id"
    "attribute.ref"           = "assertion.ref"
    "attribute.ref_protected" = "assertion.ref_protected"
  }

  oidc {
    issuer_uri        = var.gitlab_issuer_url
    allowed_audiences = [trimsuffix(var.gitlab_issuer_url, "/")]
  }

  lifecycle {
    prevent_destroy = true
  }
}

resource "google_service_account" "hosting_deployer" {
  project         = google_project.notifications.project_id
  account_id      = "louiselm-hosting-deployer"
  display_name    = "LouiseLM Firebase Hosting deployer"
  deletion_policy = "PREVENT"

  depends_on = [google_project_service.required]

  lifecycle {
    prevent_destroy = true
  }
}

locals {
  hosting_deployer_roles = toset([
    "roles/firebasehosting.admin",
    "roles/serviceusage.apiKeysViewer",
  ])
}

resource "google_project_iam_member" "hosting_deployer" {
  for_each = local.hosting_deployer_roles

  project = google_project.notifications.project_id
  role    = each.value
  member  = "serviceAccount:${google_service_account.hosting_deployer.email}"

  depends_on = [google_firebase_project.notifications]
}

resource "google_service_account_iam_member" "hosting_deployer_workload_identity" {
  service_account_id = google_service_account.hosting_deployer.name
  role               = "roles/iam.workloadIdentityUser"
  member             = "principalSet://iam.googleapis.com/${google_iam_workload_identity_pool.gitlab.name}/attribute.environment/site-production"
}
