#!/bin/sh
set -eu
umask 077

infra_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
test_dir=$(mktemp -d)
trap 'find "$test_dir" -type f -delete; rmdir "$test_dir"' EXIT HUP INT TERM

# Synthetic tofu-show JSON: no credentials, state, or real plan is used in CI.
jq -n '{
  format_version: "1.2", terraform_version: "1.12.3", errored: false,
  resource_changes: [],
  planned_values: {root_module: {resources: [
    {address: "google_project_iam_member.hosting_deployer[\"roles/firebasehosting.admin\"]",
     mode: "managed", type: "google_project_iam_member", values: {
       project: "louiselm", role: "roles/firebasehosting.admin",
       member: "serviceAccount:louiselm-hosting-deployer@louiselm.iam.gserviceaccount.com"}},
    {address: "google_project_iam_member.hosting_deployer[\"roles/serviceusage.apiKeysViewer\"]",
     mode: "managed", type: "google_project_iam_member", values: {
       project: "louiselm", role: "roles/serviceusage.apiKeysViewer",
       member: "serviceAccount:louiselm-hosting-deployer@louiselm.iam.gserviceaccount.com"}},
    {address: "google_project_iam_member.sender",
     mode: "managed", type: "google_project_iam_member", values: {
       project: "louiselm", role: "projects/louiselm/roles/louiselmFcmSender",
       member: "serviceAccount:louiselm-fcm-sender@louiselm.iam.gserviceaccount.com"}},
    {address: "google_project_iam_custom_role.sender",
     mode: "managed", type: "google_project_iam_custom_role", values: {
       project: "louiselm", role_id: "louiselmFcmSender",
       permissions: ["cloudmessaging.messages.create"]}}
  ]}}
}' > "$test_dir/baseline.json"

check() {
  label=$1 expected=$2 filter=$3
  jq "$filter" "$test_dir/baseline.json" > "$test_dir/plan.json"
  actual=0
  sh "$infra_dir/check-plan-iam.sh" "$test_dir/plan.json" > "$test_dir/output" 2>&1 || actual=$?
  if [ "$actual" -ne "$expected" ]; then
    printf 'FAIL: %s (expected %s, got %s)\n' "$label" "$expected" "$actual" >&2
    exit 1
  fi
  # Diagnostics must not echo plan values, including jq parse errors.
  if grep -q 'PRIVATE-PLAN-MARKER' "$test_dir/output"; then
    printf 'FAIL: %s leaked plan data\n' "$label" >&2
    exit 1
  fi
  printf 'PASS: %s\n' "$label"
}

check 'reviewed Hosting and send-only grants' 0 '.'
for role in owner editor viewer apikeys.admin firebase.admin iam.roleAdmin iam.serviceAccountAdmin iam.workloadIdentityPoolAdmin resourcemanager.projectIamAdmin serviceusage.serviceUsageAdmin; do
  check "resolved dynamic roles/$role grant" 1 ".planned_values.root_module.resources[0] |= (.values.role = \"roles/$role\" | .address = (\"google_project_iam_member.hosting_deployer[\" + (.values.role | tojson) + \"]\"))"
done
check 'new admin role, not just the original denylist' 1 '.planned_values.root_module.resources[0].values.role = "roles/storage.admin"'
check 'Hosting exception does not cover other accounts' 1 '.planned_values.root_module.resources[0].values.member = "serviceAccount:PRIVATE-PLAN-MARKER@example.com"'
check 'Hosting exception does not cover state project' 1 '.planned_values.root_module.resources[0].values.project = "louiselm-state"'
check 'Hosting exception does not cover another resource' 1 '.planned_values.root_module.resources[0].address = "google_project_iam_member.other"'
check 'sender exception does not cover other roles' 1 '.planned_values.root_module.resources[2].values.role = "roles/editor"'
check 'custom sender role cannot gain permissions' 1 '.planned_values.root_module.resources[3].values.permissions += ["resourcemanager.projects.setIamPolicy"]'
check 'binding cannot bypass member policy' 1 '.planned_values.root_module.resources[0].type = "google_project_iam_binding"'
check 'authoritative policy cannot bypass member policy' 1 '.planned_values.root_module.resources[0].type = "google_project_iam_policy"'
check 'unknown role fails closed' 1 'del(.planned_values.root_module.resources[0].values.role)'
check 'unknown member fails closed' 1 'del(.planned_values.root_module.resources[0].values.member)'
check 'nested module cannot hide a grant' 1 '.planned_values.root_module |= {child_modules: [{address: "module.other", resources: [.resources[0] | .address = "module.other.google_project_iam_member.admin"]}]}'
check 'data lookups do not grant roles' 0 '.planned_values.root_module.resources += [{mode: "data", type: "google_project_iam_policy", address: "data.google_project_iam_policy.read", values: {}}]'
check 'unrelated resources are left to other gates and review' 0 '.planned_values.root_module.resources += [{mode: "managed", type: "google_storage_bucket", address: "google_storage_bucket.state", values: {}}]'
check 'absent IAM after removal is allowed' 0 '.planned_values.root_module.resources = []'
check 'empty object is not a plan' 1 '{}'
check 'state JSON is not a plan' 1 'del(.resource_changes) | .values = .planned_values | del(.planned_values)'
check 'errored plan fails' 1 '.errored = true'
check 'unsupported plan format fails' 1 '.format_version = "2.0"'
check 'malformed module fails' 1 '.planned_values.root_module.child_modules = "PRIVATE-PLAN-MARKER"'
check 'multiple JSON documents fail' 1 '., .'
check 'empty input fails' 1 'empty'

printf '{"PRIVATE-PLAN-MARKER":\n' > "$test_dir/plan.json"
if sh "$infra_dir/check-plan-iam.sh" "$test_dir/plan.json" > "$test_dir/output" 2>&1; then
  echo 'FAIL: invalid JSON was accepted' >&2
  exit 1
fi
if grep -q 'PRIVATE-PLAN-MARKER' "$test_dir/output"; then
  echo 'FAIL: invalid JSON leaked plan data' >&2
  exit 1
fi
echo 'PASS: invalid JSON rejected without printing plan data'

# Exercise the same gate against actual OpenTofu plan JSON and the module's
# full mocked-provider suite, not only a hand-written approximation of a plan.
if ! (cd "$infra_dir" && tofu test -json -verbose) > "$test_dir/tofu.jsonl"; then
  jq -r 'select(.type == "diagnostic") | .diagnostic.summary' "$test_dir/tofu.jsonl" >&2
  echo 'FAIL: native OpenTofu tests; run tofu test for diagnostics' >&2
  exit 1
fi
jq -e 'select(.type == "test_plan" and .["@testrun"] == "app_identity_bindings_stay_in_app_project") | .test_plan' \
  "$test_dir/tofu.jsonl" > "$test_dir/plan.json"
sh "$infra_dir/check-plan-iam.sh" "$test_dir/plan.json"
jq '.planned_values.root_module.resources += [{
  address: "google_project_iam_member.ci[\"roles/owner\"]",
  mode: "managed", type: "google_project_iam_member",
  values: {project: "louiselm", role: "roles/owner", member: "serviceAccount:ci@louiselm.iam.gserviceaccount.com"}
}]' "$test_dir/plan.json" > "$test_dir/privileged-plan.json"
if sh "$infra_dir/check-plan-iam.sh" "$test_dir/privileged-plan.json" > "$test_dir/output" 2>&1; then
  echo 'FAIL: extra privileged grant in actual mock plan was accepted' >&2
  exit 1
fi
jq -r 'select(.type == "test_summary") | .["@message"]' "$test_dir/tofu.jsonl"
echo 'PASS: actual OpenTofu mock plan passed the production IAM gate'
echo 'PASS: extra privileged grant in actual mock plan rejected'
