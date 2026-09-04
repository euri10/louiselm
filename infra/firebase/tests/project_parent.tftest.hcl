mock_provider "google" {
  mock_resource "google_service_account" {
    defaults = {
      name = "projects/louiselm-test-project/serviceAccounts/louiselm-infra-ci@louiselm-test-project.iam.gserviceaccount.com"
    }
  }
}

mock_provider "google-beta" {}

variables {
  project_id               = "louiselm-test-project"
  billing_account_id       = "ABCDEF-123456-ABCDEF"
  android_sha1_fingerprint = "0123456789abcdef0123456789abcdef01234567"
  gitlab_issuer_url        = "https://gitlab.example.com/"
  gitlab_project_id        = "163"
}

run "parentless_project" {
  command = plan

  assert {
    condition     = google_project.notifications.org_id == null && google_project.notifications.folder_id == null
    error_message = "A dedicated project must support Google Cloud accounts without an organization or folder."
  }
}

run "organization_parent" {
  command = plan

  variables {
    org_id = "123456789"
  }

  assert {
    condition     = google_project.notifications.org_id == "123456789" && google_project.notifications.folder_id == null
    error_message = "A dedicated project must support an organization parent without a folder parent."
  }
}

run "folder_parent" {
  command = plan

  variables {
    folder_id = "987654321"
  }

  assert {
    condition     = google_project.notifications.org_id == null && google_project.notifications.folder_id == "987654321"
    error_message = "A dedicated project must support a folder parent without an organization parent."
  }
}

run "both_parents" {
  command = plan

  variables {
    org_id    = "123456789"
    folder_id = "987654321"
  }

  expect_failures = [google_project.notifications]
}

run "wif_requires_branch_ref" {
  command = plan

  assert {
    condition     = strcontains(google_iam_workload_identity_pool_provider.gitlab.attribute_condition, "assertion.ref_type == 'branch'")
    error_message = "GitLab federation must distinguish the protected main branch from a protected tag with the same short ref."
  }
}
