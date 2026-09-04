#!/bin/sh
set -eu

# The pinned GitLab component runs this from TF_PROJECT_DIR before every init.
if grep -Eq 'roles/(apikeys\.admin|firebase\.admin|iam\.roleAdmin|iam\.serviceAccountAdmin|iam\.workloadIdentityPoolAdmin|resourcemanager\.projectIamAdmin|serviceusage\.serviceUsageAdmin|viewer)' ./*.tf; then
  echo 'Firebase infrastructure is operator-owned; broad infrastructure CI roles are forbidden.' >&2
  exit 1
fi

if ! awk '
  /^provider "google-beta" \{$/ { in_firebase_provider = 1; next }
  in_firebase_provider && /^[[:space:]]*billing_project[[:space:]]*=[[:space:]]*var\.project_id[[:space:]]*$/ { billing_project = 1 }
  in_firebase_provider && /^[[:space:]]*user_project_override[[:space:]]*=[[:space:]]*true[[:space:]]*$/ { user_project_override = 1 }
  in_firebase_provider && /^}$/ { in_firebase_provider = 0 }
  END { exit !(billing_project && user_project_override) }
' versions.tf; then
  echo 'The Firebase provider must charge API requests to the isolated app project.' >&2
  exit 1
fi

if grep -Eq '^[[:space:]]*backend[[:space:]]+"gcs"[[:space:]]*\{' versions.tf &&
  grep -Eq '^[[:space:]]*bucket[[:space:]]*=[[:space:]]*"louiselm-tfstate"[[:space:]]*$' versions.tf &&
  grep -Eq '^[[:space:]]*prefix[[:space:]]*=[[:space:]]*"production"[[:space:]]*$' versions.tf; then
  exit 0
fi

echo 'Firebase infrastructure requires the protected louiselm-tfstate GCS backend.' >&2
exit 1
