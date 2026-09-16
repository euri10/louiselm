resource "google_apikeys_key" "android_qa" {
  count    = var.android_qa_sha1_fingerprint == null ? 0 : 1
  provider = google-beta

  name            = "louiselm-android-qa"
  display_name    = "LouiseLM QA Android Firebase key"
  project         = google_project.notifications.project_id
  deletion_policy = "PREVENT"

  restrictions {
    android_key_restrictions {
      allowed_applications {
        package_name     = "dev.louiselm.capture.qa"
        sha1_fingerprint = lower(var.android_qa_sha1_fingerprint)
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

resource "google_firebase_android_app" "qa" {
  count    = var.android_qa_sha1_fingerprint == null ? 0 : 1
  provider = google-beta

  project         = google_project.notifications.project_id
  display_name    = "LouiseLM QA Android"
  package_name    = "dev.louiselm.capture.qa"
  sha1_hashes     = [lower(var.android_qa_sha1_fingerprint)]
  api_key_id      = google_apikeys_key.android_qa[0].uid
  deletion_policy = "PREVENT"

  depends_on = [google_firebase_project.notifications]
}

data "google_firebase_android_app_config" "qa" {
  count    = var.android_qa_sha1_fingerprint == null ? 0 : 1
  provider = google-beta

  project = google_project.notifications.project_id
  app_id  = google_firebase_android_app.qa[0].app_id
}
