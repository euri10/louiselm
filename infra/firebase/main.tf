locals {
  required_services = toset([
    "apikeys.googleapis.com",
    "cloudbilling.googleapis.com",
    "cloudresourcemanager.googleapis.com",
    "fcm.googleapis.com",
    "firebase.googleapis.com",
    "firebaseinstallations.googleapis.com",
    "iam.googleapis.com",
    "iamcredentials.googleapis.com",
    "serviceusage.googleapis.com",
  ])
}

resource "google_project" "notifications" {
  project_id      = var.project_id
  name            = var.project_name
  billing_account = var.billing_account_id
  org_id          = var.org_id
  folder_id       = var.folder_id
  deletion_policy = "PREVENT"

  lifecycle {
    prevent_destroy = true
  }
}

resource "google_project_service" "required" {
  for_each = local.required_services

  project            = google_project.notifications.project_id
  service            = each.value
  disable_on_destroy = false
  deletion_policy    = "PREVENT"
}

resource "google_firebase_project" "notifications" {
  provider = google-beta
  project  = google_project.notifications.project_id

  lifecycle {
    prevent_destroy = true
  }

  depends_on = [google_project_service.required]
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
      service = "fcm.googleapis.com"
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

  lifecycle {
    prevent_destroy = true
  }
}

resource "google_project_iam_member" "sender" {
  project = google_project.notifications.project_id
  role    = google_project_iam_custom_role.sender.name
  member  = "serviceAccount:${google_service_account.sender.email}"
}

resource "google_iam_workload_identity_pool" "gitlab" {
  project                   = google_project.notifications.project_id
  workload_identity_pool_id = var.gitlab_workload_identity_pool_id
  display_name              = "LouiseLM GitLab CI"
  description               = "Keyless protected-main CI access for LouiseLM infrastructure."
  disabled                  = false
  deletion_policy           = "PREVENT"

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
  attribute_condition                = "assertion.project_id == '${var.gitlab_project_id}' && assertion.ref == '${var.gitlab_default_branch}' && assertion.ref_protected == 'true'"
  attribute_mapping = {
    "google.subject"          = "assertion.sub"
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

resource "google_service_account" "ci" {
  project         = google_project.notifications.project_id
  account_id      = "louiselm-infra-ci"
  display_name    = "LouiseLM infrastructure CI"
  deletion_policy = "PREVENT"

  lifecycle {
    prevent_destroy = true
  }
}

locals {
  ci_roles = toset([
    "roles/apikeys.admin",
    "roles/firebase.admin",
    "roles/iam.roleAdmin",
    "roles/iam.serviceAccountAdmin",
    "roles/iam.workloadIdentityPoolAdmin",
    "roles/resourcemanager.projectIamAdmin",
    "roles/serviceusage.serviceUsageAdmin",
    "roles/viewer",
  ])
}

resource "google_project_iam_member" "ci" {
  for_each = local.ci_roles

  project = google_project.notifications.project_id
  role    = each.value
  member  = "serviceAccount:${google_service_account.ci.email}"
}

resource "google_service_account_iam_member" "ci_workload_identity" {
  service_account_id = google_service_account.ci.name
  role               = "roles/iam.workloadIdentityUser"
  member             = "principalSet://iam.googleapis.com/${google_iam_workload_identity_pool.gitlab.name}/attribute.project_id/${var.gitlab_project_id}"
}
