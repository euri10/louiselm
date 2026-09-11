#!/bin/sh
set -eu

if [ "$#" -ne 1 ] || [ ! -f "$1" ]; then
  echo 'Usage: sh check-plan-iam.sh <tofu-show-plan.json>' >&2
  exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
  echo 'Project IAM plan gate requires jq (also required by the operator runbook).' >&2
  exit 1
fi

# Inspect resolved values, not HCL expressions. Keep diagnostics payload-free:
# plan JSON contains sensitive values even when tofu marks them sensitive.
if ! jq -es '
  def modules: recurse((.child_modules // [])[]);
  def reviewed_project_iam:
    .values as $v |
    $v.project == "louiselm" and (
      if .type == "google_project_iam_member" then
        (
          .address == ("google_project_iam_member.hosting_deployer[" + ($v.role | tojson) + "]") and
          ($v.role == "roles/firebasehosting.admin" or $v.role == "roles/serviceusage.apiKeysViewer") and
          $v.member == "serviceAccount:louiselm-hosting-deployer@louiselm.iam.gserviceaccount.com"
        ) or (
          .address == "google_project_iam_member.sender" and
          $v.role == "projects/louiselm/roles/louiselmFcmSender" and
          $v.member == "serviceAccount:louiselm-fcm-sender@louiselm.iam.gserviceaccount.com"
        )
      elif .type == "google_project_iam_custom_role" then
        .address == "google_project_iam_custom_role.sender" and
        $v.role_id == "louiselmFcmSender" and
        $v.permissions == ["cloudmessaging.messages.create"]
      else false end
    );

  length == 1 and (.[0] |
    (.format_version | type == "string" and startswith("1.")) and
    .errored != true and
    (.resource_changes | type == "array") and
    (.planned_values.root_module | type == "object") and
    all(.planned_values.root_module | modules;
      type == "object" and
      ((.resources // []) | type == "array") and
      ((.child_modules // []) | type == "array")
    ) and
    all(.planned_values.root_module | modules | (.resources // [])[];
      (.type | type == "string") and
      (.values | type == "object") and
      (.mode == "data" or (.mode == "managed" and (
        if (.type | test("^google_project_iam_(member|binding|policy|custom_role)$"))
        then reviewed_project_iam else true end
      )))
    )
  )
' -- "$1" >/dev/null 2>&1; then
  echo 'Project IAM plan gate failed: invalid/unresolved plan or unreviewed project IAM grant/custom role.' >&2
  echo 'Review the saved plan privately; do not apply or bypass the gate. See infra/firebase/README.md.' >&2
  exit 1
fi

echo 'Project IAM plan gate passed (reviewed project grants and send-only custom role).'
