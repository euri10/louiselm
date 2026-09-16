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
}

run "qa_unset_preserves_normal_app" {
  command = plan

  assert {
    condition = (
      length(google_firebase_android_app.qa) == 0 &&
      length(google_apikeys_key.android_qa) == 0 &&
      length(data.google_firebase_android_app_config.qa) == 0 &&
      output.android_qa_firebase_config_json == null
    )
    error_message = "Omitting QA signing configuration must create no QA resources or client output."
  }

  assert {
    condition = (
      google_firebase_android_app.notifications.package_name == "dev.louiselm.capture" &&
      google_firebase_android_app.notifications.sha1_hashes == tolist([var.android_sha1_fingerprint]) &&
      google_apikeys_key.android.restrictions[0].android_key_restrictions[0].allowed_applications == tolist([{
        package_name     = "dev.louiselm.capture"
        sha1_fingerprint = var.android_sha1_fingerprint
      }])
    )
    error_message = "QA support must not change the existing normal app or broaden its key."
  }
}

run "reject_empty_qa_fingerprint" {
  command = plan

  variables {
    android_qa_sha1_fingerprint = ""
  }

  expect_failures = [var.android_qa_sha1_fingerprint]
}

run "reject_short_qa_fingerprint" {
  command = plan

  variables {
    android_qa_sha1_fingerprint = "abcdef"
  }

  expect_failures = [var.android_qa_sha1_fingerprint]
}

run "reject_non_hex_qa_fingerprint" {
  command = plan

  variables {
    android_qa_sha1_fingerprint = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"
  }

  expect_failures = [var.android_qa_sha1_fingerprint]
}

run "qa_uses_its_own_restricted_key" {
  # Overrides supply known IDs at plan time. Keep this plan-only:
  # mock apply teardown also honors production prevent_destroy protections.
  command = plan

  variables {
    android_qa_sha1_fingerprint = "ABCDEF0123456789ABCDEF0123456789ABCDEF01"
  }

  override_resource {
    target = google_apikeys_key.android
    values = {
      uid = "11111111-1111-4111-8111-111111111111"
    }
  }

  override_resource {
    target = google_apikeys_key.android_qa
    values = {
      uid = "22222222-2222-4222-8222-222222222222"
    }
  }

  override_resource {
    target = google_firebase_android_app.qa
    values = {
      app_id = "1:123456789012:android:0123456789abcdef"
    }
  }

  assert {
    condition = (
      length(google_firebase_android_app.qa) == 1 &&
      length(google_apikeys_key.android_qa) == 1 &&
      google_firebase_android_app.qa[0].project == "louiselm" &&
      google_firebase_android_app.qa[0].package_name == "dev.louiselm.capture.qa" &&
      google_firebase_android_app.qa[0].sha1_hashes == tolist([lower(var.android_qa_sha1_fingerprint)]) &&
      google_firebase_android_app.qa[0].api_key_id == google_apikeys_key.android_qa[0].uid &&
      google_firebase_android_app.qa[0].deletion_policy == "PREVENT" &&
      google_apikeys_key.android_qa[0].project == "louiselm" &&
      google_apikeys_key.android_qa[0].name == "louiselm-android-qa" &&
      google_apikeys_key.android_qa[0].deletion_policy == "PREVENT"
    )
    error_message = "QA must have its own protected app and key, bound to its signing certificate in the existing project."
  }

  assert {
    condition = (
      google_apikeys_key.android_qa[0].restrictions[0].android_key_restrictions[0].allowed_applications == tolist([{
        package_name     = "dev.louiselm.capture.qa"
        sha1_fingerprint = lower(var.android_qa_sha1_fingerprint)
      }]) &&
      toset([for target in google_apikeys_key.android_qa[0].restrictions[0].api_targets : target.service]) == toset([
        "firebase.googleapis.com",
        "fcmregistrations.googleapis.com",
        "firebaseinstallations.googleapis.com",
        "logging.googleapis.com",
      ])
    )
    error_message = "QA's key must allow only its package/certificate and the required Firebase client APIs."
  }

  assert {
    condition = (
      google_firebase_android_app.notifications.package_name == "dev.louiselm.capture" &&
      google_firebase_android_app.notifications.sha1_hashes == tolist([var.android_sha1_fingerprint]) &&
      google_firebase_android_app.notifications.api_key_id == google_apikeys_key.android.uid &&
      google_apikeys_key.android.uid != google_apikeys_key.android_qa[0].uid &&
      google_apikeys_key.android.restrictions[0].android_key_restrictions[0].allowed_applications == tolist([{
        package_name     = "dev.louiselm.capture"
        sha1_fingerprint = var.android_sha1_fingerprint
      }]) &&
      google_project_iam_custom_role.sender.permissions == toset(["cloudmessaging.messages.create"])
    )
    error_message = "Enabling QA must preserve the normal app/key and send-only IAM permission."
  }

  assert {
    condition = (
      data.google_firebase_android_app_config.qa[0].project == "louiselm" &&
      data.google_firebase_android_app_config.qa[0].app_id == google_firebase_android_app.qa[0].app_id
    )
    error_message = "QA configuration must be fetched for the QA app, not the normal app."
  }
}
