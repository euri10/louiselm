output "project_id" {
  description = "Dedicated Firebase/GCP project ID."
  value       = google_project.notifications.project_id
}

output "project_number" {
  description = "Dedicated Firebase/GCP project number."
  value       = google_project.notifications.number
}

output "android_app_id" {
  description = "Firebase-assigned Android application ID."
  value       = google_firebase_android_app.notifications.app_id
}

output "android_api_key" {
  description = "Restricted Android Firebase API key; retrieve only into a protected local file."
  value       = google_apikeys_key.android.key_string
  sensitive   = true
}

module "android_config" {
  source = "./modules/android-config"

  config_file_contents = data.google_firebase_android_app_config.notifications.config_file_contents
  package_name         = google_firebase_android_app.notifications.package_name
  api_key              = google_apikeys_key.android.key_string
}

module "android_qa_config" {
  count  = var.android_qa_sha1_fingerprint == null ? 0 : 1
  source = "./modules/android-config"

  config_file_contents = data.google_firebase_android_app_config.qa[0].config_file_contents
  package_name         = google_firebase_android_app.qa[0].package_name
  api_key              = google_apikeys_key.android_qa[0].key_string
}

output "android_firebase_config_json" {
  description = "google-services.json containing only the normal client and its restricted key; never commit or print this output."
  value       = module.android_config.config_json
  sensitive   = true
}

output "android_qa_firebase_config_json" {
  description = "google-services.json containing only the QA client and its restricted key, or null when unconfigured; never commit or print this output."
  value       = var.android_qa_sha1_fingerprint == null ? null : module.android_qa_config[0].config_json
  sensitive   = true
}

output "sender_service_account_email" {
  description = "Runtime sender service account email; no private key is created here."
  value       = google_service_account.sender.email
}

output "sender_role" {
  description = "Least-privilege custom role granted to the runtime sender."
  value       = google_project_iam_custom_role.sender.name
}

output "gitlab_workload_identity_provider" {
  description = "Provider resource name to configure as GCP_OIDC_PROVIDER in GitLab."
  value       = google_iam_workload_identity_pool_provider.gitlab.name
}

output "hosting_site_id" {
  description = "Firebase Hosting site ID."
  value       = google_firebase_hosting_site.public.site_id
}

output "hosting_default_url" {
  description = "Default Firebase Hosting URL before the custom domain is connected."
  value       = google_firebase_hosting_site.public.default_url
}

output "hosting_deployer_service_account_email" {
  description = "Site-production GitLab deploy service account email."
  value       = google_service_account.hosting_deployer.email
}
