mock_provider "google" {
  mock_resource "google_service_account" {
    defaults = {
      name = "projects/louiselm/serviceAccounts/mock-account@louiselm.iam.gserviceaccount.com"
    }
  }

  mock_resource "google_iam_workload_identity_pool" {
    defaults = {
      name = "projects/louiselm/locations/global/workloadIdentityPools/gitlab"
    }
  }
}

mock_provider "google-beta" {}

variables {
  billing_account_id       = "ABCDEF-123456-ABCDEF"
  android_sha1_fingerprint = "0123456789abcdef0123456789abcdef01234567"
  gitlab_issuer_url        = "https://gitlab.bartab.fr/"
  gitlab_project_id        = "163"
}

run "reject_other_project_id" {
  command = plan

  variables {
    project_id = "another-project"
  }

  expect_failures = [var.project_id]
}

run "reject_other_state_project_id" {
  command = plan

  variables {
    state_project_id = "another-state-project"
  }

  expect_failures = [var.state_project_id]
}

run "reject_other_gitlab_issuer" {
  command = plan

  variables {
    gitlab_issuer_url = "https://gitlab.example.com/"
  }

  expect_failures = [var.gitlab_issuer_url]
}

run "reject_other_gitlab_project" {
  command = plan

  variables {
    gitlab_project_id = "999"
  }

  expect_failures = [var.gitlab_project_id]
}

run "reject_other_gitlab_branch" {
  command = plan

  variables {
    gitlab_default_branch = "develop"
  }

  expect_failures = [var.gitlab_default_branch]
}

run "parentless_project" {
  command = plan

  assert {
    condition = (
      google_project.notifications.org_id == null &&
      google_project.notifications.folder_id == null &&
      google_project.state.org_id == null &&
      google_project.state.folder_id == null
    )
    error_message = "Both dedicated projects must support Google Cloud accounts without an organization or folder."
  }
}

run "organization_parent" {
  command = plan

  variables {
    org_id = "123456789"
  }

  assert {
    condition = (
      google_project.notifications.org_id == "123456789" &&
      google_project.notifications.folder_id == null &&
      google_project.state.org_id == "123456789" &&
      google_project.state.folder_id == null
    )
    error_message = "Both dedicated projects must support an organization parent without a folder parent."
  }
}

run "folder_parent" {
  command = plan

  variables {
    folder_id = "987654321"
  }

  assert {
    condition = (
      google_project.notifications.org_id == null &&
      google_project.notifications.folder_id == "987654321" &&
      google_project.state.org_id == null &&
      google_project.state.folder_id == "987654321"
    )
    error_message = "Both dedicated projects must support a folder parent without an organization parent."
  }
}

run "both_parents" {
  command = plan

  variables {
    org_id    = "123456789"
    folder_id = "987654321"
  }

  expect_failures = [google_project.notifications, google_project.state]
}

run "wif_requires_branch_ref" {
  command = plan

  assert {
    condition     = strcontains(google_iam_workload_identity_pool_provider.gitlab.attribute_condition, "assertion.ref_type == 'branch'")
    error_message = "GitLab federation must distinguish the protected main branch from a protected tag with the same short ref."
  }
}

run "site_deploy_only_federation" {
  command = plan

  assert {
    condition     = google_iam_workload_identity_pool_provider.gitlab.attribute_condition == "assertion.project_id == '163' && assertion.ref_type == 'branch' && assertion.ref == 'main' && assertion.ref_protected == 'true' && assertion.environment == 'site-production'"
    error_message = "GitLab federation must authorize only the narrow site-production deployment boundary."
  }

  assert {
    condition     = google_iam_workload_identity_pool_provider.gitlab.attribute_mapping["attribute.environment"] == "assertion.environment"
    error_message = "GitLab federation must map the environment claim used by the Hosting service-account binding."
  }

  assert {
    condition     = google_service_account_iam_member.hosting_deployer_workload_identity.member == "principalSet://iam.googleapis.com/projects/louiselm/locations/global/workloadIdentityPools/gitlab/attribute.environment/site-production"
    error_message = "Only the site-production principal set may impersonate the Hosting deployer."
  }
}

run "isolated_state_project" {
  command = plan

  assert {
    condition = (
      var.project_id == "louiselm" &&
      var.state_project_id == "louiselm-state" &&
      var.project_id != var.state_project_id
    )
    error_message = "The Firebase and state projects must use distinct fixed project IDs."
  }

  assert {
    condition = (
      !contains(local.required_services, "storage.googleapis.com") &&
      local.state_required_services == toset([
        "cloudresourcemanager.googleapis.com",
        "policytroubleshooter.googleapis.com",
        "serviceusage.googleapis.com",
        "storage.googleapis.com",
      ])
    )
    error_message = "Cloud Storage and the state bootstrap APIs must be isolated to the state project."
  }

  assert {
    condition = (
      google_storage_bucket.state.name == "louiselm-tfstate" &&
      google_project.state.name == "LouiseLM State" &&
      google_project.notifications.name == "LouiseLM" &&
      google_storage_bucket.state.project == google_project.state.project_id &&
      google_storage_bucket.state.location == "EU" &&
      google_storage_bucket.state.uniform_bucket_level_access &&
      google_storage_bucket.state.public_access_prevention == "enforced" &&
      !google_storage_bucket.state.force_destroy &&
      google_storage_bucket.state.versioning[0].enabled &&
      google_storage_bucket.state.soft_delete_policy[0].retention_duration_seconds >= 604800 &&
      google_storage_bucket.state.deletion_policy == "PREVENT"
    )
    error_message = "The isolated state bucket must retain private, versioned, soft-deleted state and reject deletion."
  }

  assert {
    condition = (
      google_resource_manager_lien.state.parent == "projects/${var.state_project_id}" &&
      contains(google_resource_manager_lien.state.restrictions, "resourcemanager.projects.delete") &&
      google_resource_manager_lien.state.deletion_policy == "PREVENT" &&
      google_resource_manager_lien.notifications.parent == "projects/${var.project_id}" &&
      contains(google_resource_manager_lien.notifications.restrictions, "resourcemanager.projects.delete") &&
      google_resource_manager_lien.notifications.deletion_policy == "PREVENT"
    )
    error_message = "Deletion-protected liens must independently guard the state and Firebase projects."
  }

  assert {
    condition = (
      google_project_iam_audit_config.state.project == google_project.state.project_id &&
      google_project_iam_audit_config.notifications.project == google_project.notifications.project_id
    )
    error_message = "Each project must keep its own Data Access audit configuration."
  }
}

run "app_identity_bindings_stay_in_app_project" {
  command = plan

  assert {
    condition = local.hosting_deployer_roles == toset([
      "roles/firebasehosting.admin",
      "roles/serviceusage.apiKeysViewer",
    ])
    error_message = "The GitLab deployer must retain only Firebase's Hosting roles and no state access."
  }

  assert {
    condition = (
      alltrue([
        for binding in values(google_project_iam_member.hosting_deployer) :
        binding.project == google_project.notifications.project_id
      ]) &&
      google_project_iam_member.sender.project == google_project.notifications.project_id &&
      google_iam_workload_identity_pool.gitlab.project == google_project.notifications.project_id
    )
    error_message = "Hosting, runtime, and federation identities must remain entirely inside the Firebase project."
  }
}

run "fcm_client_api_key_allowlist" {
  command = plan

  assert {
    condition = alltrue([
      for service in [
        "fcm.googleapis.com",
        "fcmregistrations.googleapis.com",
        "firebase.googleapis.com",
        "firebaseinstallations.googleapis.com",
        "logging.googleapis.com",
      ] : contains(local.required_services, service)
    ])
    error_message = "The project must enable the server send API and every client API required by Firebase Cloud Messaging."
  }

  assert {
    condition = toset([
      for target in google_apikeys_key.android.restrictions[0].api_targets : target.service
      ]) == toset([
      "fcmregistrations.googleapis.com",
      "firebase.googleapis.com",
      "firebaseinstallations.googleapis.com",
      "logging.googleapis.com",
    ])
    error_message = "The Android API key must allow exactly the Firebase client APIs required by Cloud Messaging."
  }
}

run "app_compute_is_managed_and_deprivileged" {
  command = plan

  assert {
    condition = (
      contains(local.required_services, "compute.googleapis.com") &&
      google_project_default_service_accounts.app.project == google_project.notifications.project_id &&
      google_project_default_service_accounts.app.action == "DEPRIVILEGE" &&
      google_project_default_service_accounts.app.restore_policy == "NONE"
    )
    error_message = "The app default-network inspection API must be managed and its default service accounts deprivileged."
  }
}

run "wif_required_services" {
  command = plan

  assert {
    condition     = contains(local.required_services, "sts.googleapis.com")
    error_message = "Workload identity federation requires the Security Token Service API."
  }
}
