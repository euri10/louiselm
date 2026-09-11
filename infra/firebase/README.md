# LouiseLM Firebase foundation

This composition manages two deletion-protected Google Cloud projects:

| Project | Purpose | LouiseLM identity with access |
| --- | --- | --- |
| `louiselm-state` | GCS backend and its audit/protection resources | operator |
| `louiselm` | Firebase Cloud Messaging, Android app, Hosting, and deploy/runtime service accounts | operator plus the narrow Hosting deployer |

State lives at
`gs://louiselm-tfstate/production/default.tfstate`. Keeping the bucket in the
separate, currently parentless state project prevents Firebase-created service
agents from inheriting access to it. GitLab CI has no infrastructure identity
or state access; only a protected `site-production` job may impersonate the
Hosting deployer. Recheck ancestor IAM if either project later gains a parent.

The composition creates no service-account private key, FCM server secret,
device token, or generated Android configuration file. Firebase configuration
is available only as a sensitive output for an operator to install out of band.

## Fixed identities and inputs

The globally unique project IDs are fixed to `louiselm-state` and `louiselm`,
and the globally unique bucket name is fixed to `louiselm-tfstate`. If any name
cannot be reserved, stop and change the reviewed configuration and runbook
together; never substitute a name during bootstrap. Deleted project IDs cannot
be reused.

Copy `terraform.tfvars.example` to an ignored, mode-0600 `terraform.tfvars` and
fill in the billing account and Android signing certificate fingerprint. The
bootstrap below is for the confirmed parentless account and requires both
`org_id` and `folder_id` to remain unset. If a parent becomes available, update
and re-review the create commands and assertions with exactly one parent; the
same parent applies to both projects. Never commit tfvars, state, plans,
credentials, `google-services.json`, or Firebase CLI tokens.

OpenTofu state can contain sensitive values even when CLI output masks them.
Infrastructure plan and apply are therefore operator-only and always use a
saved, reviewed plan. This is a production-critical composition using OpenTofu
1.12.3, `hashicorp/google` 7.40.0, `hashicorp/google-beta` 7.40.0, and the GCS
backend committed in `versions.tf`.

The state bucket uses Google-managed encryption, uniform bucket-level access,
public-access prevention, object versioning, and seven-day soft deletion. A
customer-managed key would make recovery depend on another resource in the
project being recovered. A bucket retention policy is deliberately absent
because the GCS backend must delete `production/default.tflock` when releasing
a lock. Project-wide Data Access audit logs provide supplemental access
evidence; object generations and soft deletion remain the recovery sources.

## First bootstrap

The state project and bucket must exist before the backend can initialize. The
following is a one-time operator procedure, not CI. Run it from a clean checkout
of the merged configuration in one Bash shell. It reserves permanent names and later
deletes the app default network, so stop on every failed assertion and never
retry a create or delete command blindly.

### 1. Preflight

Keep `LOUISELM_PRODUCTION_READY=false` and cancel every old pending or manual
production job before provisioning. Prepare the ignored `terraform.tfvars`,
then verify the toolchain, local state boundary, active CLI/ADC identity, and
the intentionally empty legacy GitLab state:

```bash
set +x
set -euo pipefail
test -n "${BASH_VERSION:-}"
cd infra/firebase
git fetch origin main --quiet
test "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)"
test -z "$(git status --porcelain=v1 --untracked-files=all)"
test "$(tofu version -json | jq -r .terraform_version)" = 1.12.3
test "$(gcloud version --format=json | jq -r '."Google Cloud SDK"')" = 582.0.0
test -f terraform.tfvars
test ! -L terraform.tfvars
git check-ignore -q terraform.tfvars
test "$(stat -c '%a' terraform.tfvars)" = 600
unexpected_auto_file=$(find . -maxdepth 1 \
  \( -name terraform.tfvars.json -o -name '*.auto.tfvars' -o -name '*.auto.tfvars.json' \) \
  -print -quit)
test -z "$unexpected_auto_file"
unset unexpected_auto_file
test ! -e terraform.tfstate
test ! -L terraform.tfstate
test ! -e terraform.tfstate.backup
test ! -L terraform.tfstate.backup
test ! -e errored.tfstate
test ! -L errored.tfstate
test -z "${GOOGLE_APPLICATION_CREDENTIALS:-}"
test -z "${GOOGLE_BACKEND_CREDENTIALS:-}"
test -z "${GOOGLE_CREDENTIALS:-}"
test -z "${GOOGLE_CLOUD_KEYFILE_JSON:-}"
test -z "${GCLOUD_KEYFILE_JSON:-}"
test -z "${GOOGLE_OAUTH_ACCESS_TOKEN:-}"
test -z "${GOOGLE_BACKEND_IMPERSONATE_SERVICE_ACCOUNT:-}"
test -z "${GOOGLE_IMPERSONATE_SERVICE_ACCOUNT:-}"
test -z "${GOOGLE_BACKEND_STORAGE_CUSTOM_ENDPOINT:-}"
test -z "${GOOGLE_STORAGE_CUSTOM_ENDPOINT:-}"
test -z "${GOOGLE_BACKEND_UNIVERSE_DOMAIN:-}"
test -z "${GOOGLE_CLOUD_UNIVERSE_DOMAIN:-}"
test -z "${GOOGLE_ENCRYPTION_KEY:-}"
test -z "${GOOGLE_KMS_ENCRYPTION_KEY:-}"
test -z "${GOOGLE_BILLING_PROJECT:-}"
test -z "${CLOUDSDK_AUTH_CREDENTIAL_FILE_OVERRIDE:-}"
test -z "${CLOUDSDK_AUTH_ACCESS_TOKEN:-}"
test -z "${CLOUDSDK_AUTH_ACCESS_TOKEN_FILE:-}"
test -z "${CLOUDSDK_AUTH_IMPERSONATE_SERVICE_ACCOUNT:-}"
test -z "${TF_DATA_DIR:-}"
test -z "${TF_ENCRYPTION:-}"
test -z "${TF_LOG:-}"
test -z "${TF_LOG_PATH:-}"
test -z "${TF_WORKSPACE:-}"
test -z "$(env | sed -n 's/^TF_CLI_ARGS[^=]*=.*/set/p')"
test -z "$(env | sed -n 's/^TF_VAR_[^=]*=.*/set/p')"

test -z "$(gcloud config get-value auth/impersonate_service_account 2>/dev/null)"
test -z "$(gcloud config get-value auth/access_token_file 2>/dev/null)"
test ! -L .terraform
mkdir -p .terraform
chmod 700 .terraform
test "$(stat -c '%a' .terraform)" = 700
tofu_data_dir=$(mktemp -d .terraform/bootstrap.XXXXXX)
export TF_DATA_DIR="$tofu_data_dir"
trap 'find "$tofu_data_dir" -depth -delete' EXIT

cli_email=$(gcloud auth list --filter=status:ACTIVE --format='value(account)')
adc_email=$(gcloud auth application-default print-access-token |
  python3 -c 'import json, sys, urllib.request; token = sys.stdin.read().strip(); request = urllib.request.Request("https://www.googleapis.com/oauth2/v3/userinfo", headers={"Authorization": "Bearer " + token}); print(json.load(urllib.request.urlopen(request))["email"])')
test -n "$cli_email"
test "$cli_email" = "$adc_email"
unset cli_email adc_email

tofu fmt -check -diff -recursive
tofu init -backend=false -lockfile=readonly -input=false
tofu validate
tofu test

legacy_states=$(glab opentofu state list -R oss-public/louiselm -F json) || exit 1
printf '%s' "$legacy_states" | jq -e '
  length == 1 and
  .[0].name == "production" and
  .[0].latestVersion.serial == 0 and
  .[0].latestVersion.downloadPath == ""
' >/dev/null
unset legacy_states

glab api projects/163/variables --paginate | jq -e '
  [.[] | select(.key == "LOUISELM_PRODUCTION_READY")] |
  all(.value == "false")
' >/dev/null
glab api 'projects/163/jobs?per_page=100' --paginate | jq -e '
  [.[] |
    select((.name == "site-deploy" or .name == "tf-production") and
      (.status == "created" or .status == "pending" or
       .status == "running" or .status == "manual" or
       .status == "waiting_for_resource"))] |
  length == 0
' >/dev/null
```

Use the prepared `bartab-dns-20260802` quota project only for bootstrap commands
and the GCS backend. Every bootstrap `gcloud` command names it explicitly. The
`google-beta` provider separately pins Firebase API quota to `louiselm`; this
prevents the ambient ADC quota project from charging Firebase Management calls
to the unrelated DNS project during plan/apply. The final Policy Troubleshooter
checks instead bill the state project where that API is enabled. Do not change
the default project or persist any of these choices in ADC.

```bash
quota_project=bartab-dns-20260802
state_project=louiselm-state
app_project=louiselm
state_bucket=louiselm-tfstate
export GOOGLE_CLOUD_QUOTA_PROJECT="$quota_project"

billing_account_id=$(gcloud billing projects describe "$quota_project" \
  --billing-project="$quota_project" \
  --format='value(billingAccountName.basename())')
test -n "$billing_account_id"
test "$(grep -Ec '^[[:space:]]*billing_account_id[[:space:]]*=' terraform.tfvars)" -eq 1
configured_billing_account_id=$(sed -n \
  's/^[[:space:]]*billing_account_id[[:space:]]*=[[:space:]]*"\([0-9A-Fa-f-]*\)"[[:space:]]*$/\1/p' \
  terraform.tfvars)
test "$configured_billing_account_id" = "$billing_account_id"
! grep -Eq '^[[:space:]]*(project_id|state_project_id|org_id|folder_id|gitlab_issuer_url|gitlab_project_id|gitlab_default_branch)[[:space:]]*=' \
  terraform.tfvars
unset configured_billing_account_id
```

Ensure the ignored `terraform.tfvars` uses that billing account without
printing it. The preflight only reads credentials and state metadata; it does
not create or change cloud resources.

### 2. Reserve the state project and backend

Create the parentless state project without the default Cloud APIs bundle. Any
nonzero result is a stop condition. Inspect the exact ID before deciding how to
recover; `ALREADY_EXISTS` without access means the name needs a new reviewed
configuration.

```sh
gcloud projects create "$state_project" \
  --name='LouiseLM State' \
  --no-enable-cloud-apis \
  --billing-project="$quota_project"

state_project_json=$(gcloud projects describe "$state_project" \
  --billing-project="$quota_project" --format=json) || exit 1
printf '%s' "$state_project_json" | jq -e --arg id "$state_project" '
  .projectId == $id and
  .name == "LouiseLM State" and
  .lifecycleState == "ACTIVE" and
  (.parent // null) == null
' >/dev/null
state_project_number=$(printf '%s' "$state_project_json" | jq -er .projectNumber)
unset state_project_json
```

Link the reviewed billing account and enable the backend services plus Policy
Troubleshooter for the post-apply effective-access proof:

```sh
gcloud billing projects link "$state_project" \
  --billing-account="$billing_account_id" \
  --billing-project="$quota_project"

state_billing=$(gcloud billing projects describe "$state_project" \
  --billing-project="$quota_project" --format=json) || exit 1
printf '%s' "$state_billing" | jq -e --arg account "$billing_account_id" '
  .billingEnabled == true and
  .billingAccountName == ("billingAccounts/" + $account)
' >/dev/null
unset state_billing

gcloud services enable \
  serviceusage.googleapis.com \
  storage.googleapis.com \
  cloudresourcemanager.googleapis.com \
  policytroubleshooter.googleapis.com \
  --project="$state_project" \
  --billing-project="$quota_project"

for service in serviceusage.googleapis.com storage.googleapis.com cloudresourcemanager.googleapis.com policytroubleshooter.googleapis.com; do
  test "$(gcloud services list --enabled \
    --project="$state_project" \
    --billing-project="$quota_project" \
    --filter="config.name=$service" \
    --format='value(config.name)')" = "$service"
done
```

Create the bucket once. If creation fails, describe the exact bucket and stop;
never delete or adopt a bucket whose owning project and security settings have
not been proven.

```sh
gcloud storage buckets create "gs://$state_bucket" \
  --project="$state_project" \
  --billing-project="$quota_project" \
  --location=EU \
  --default-storage-class=STANDARD \
  --uniform-bucket-level-access \
  --public-access-prevention \
  --soft-delete-duration=7d
gcloud storage buckets update "gs://$state_bucket" \
  --billing-project="$quota_project" \
  --versioning

bucket_json=$(gcloud storage buckets describe "gs://$state_bucket" \
  --billing-project="$quota_project" --raw --format=json) || exit 1
printf '%s' "$bucket_json" | jq -e \
  --arg name "$state_bucket" \
  --arg project_number "$state_project_number" '
    .name == $name and
    .projectNumber == $project_number and
    .location == "EU" and
    .storageClass == "STANDARD" and
    .iamConfiguration.uniformBucketLevelAccess.enabled == true and
    .iamConfiguration.publicAccessPrevention == "enforced" and
    .versioning.enabled == true and
    .softDeletePolicy.retentionDurationSeconds == "604800" and
    (.retentionPolicy // null) == null
  ' >/dev/null
unset bucket_json

state_objects=$(gcloud storage objects list \
  "gs://$state_bucket/production/**" \
  --billing-project="$quota_project" \
  --format='value(name)') || exit 1
test -z "$state_objects"
unset state_objects
```

Initialize the new backend with no migration: the GitLab state is serial zero
and the GCS prefix was just proven empty. Import only the bootstrap resources:

```sh
tofu init -reconfigure -lockfile=readonly -input=false
tofu import -input=false -lock-timeout=5m \
  google_project.state "$state_project"
tofu import -input=false -lock-timeout=5m \
  'google_project_service.state_required["serviceusage.googleapis.com"]' \
  "$state_project/serviceusage.googleapis.com"
tofu import -input=false -lock-timeout=5m \
  'google_project_service.state_required["storage.googleapis.com"]' \
  "$state_project/storage.googleapis.com"
tofu import -input=false -lock-timeout=5m \
  'google_project_service.state_required["cloudresourcemanager.googleapis.com"]' \
  "$state_project/cloudresourcemanager.googleapis.com"
tofu import -input=false -lock-timeout=5m \
  'google_project_service.state_required["policytroubleshooter.googleapis.com"]' \
  "$state_project/policytroubleshooter.googleapis.com"
tofu import -input=false -lock-timeout=5m \
  google_storage_bucket.state "$state_project/$state_bucket"

expected_state='google_project.state
google_project_service.state_required["cloudresourcemanager.googleapis.com"]
google_project_service.state_required["policytroubleshooter.googleapis.com"]
google_project_service.state_required["serviceusage.googleapis.com"]
google_project_service.state_required["storage.googleapis.com"]
google_storage_bucket.state'
actual_state=$(tofu state list | LC_ALL=C sort)
test "$actual_state" = "$expected_state"
unset actual_state expected_state

state_generation=$(gcloud storage objects describe \
  "gs://$state_bucket/production/default.tfstate" \
  --billing-project="$quota_project" \
  --format='value(generation)')
test -n "$state_generation"
printf 'initial GCS state generation: %s\n' "$state_generation"
```

The state lien and audit policy are intentionally created by the first reviewed
plan, immediately after these imports. The bucket remains protected by
`force_destroy=false`, versioning, soft deletion, and provider/lifecycle guards
in the meantime.

### 3. Reserve and import the Firebase project

Create the parentless app project. Again, any create error is a stop condition.
Verify its immutable identity, then import it immediately before changing
billing or services so an interruption cannot strand it outside state.

```sh
gcloud projects create "$app_project" \
  --name=LouiseLM \
  --no-enable-cloud-apis \
  --billing-project="$quota_project"

app_project_json=$(gcloud projects describe "$app_project" \
  --billing-project="$quota_project" --format=json) || exit 1
printf '%s' "$app_project_json" | jq -e --arg id "$app_project" '
  .projectId == $id and
  .name == "LouiseLM" and
  .lifecycleState == "ACTIVE" and
  (.parent // null) == null
' >/dev/null
app_project_number=$(printf '%s' "$app_project_json" | jq -er .projectNumber)
unset app_project_json

tofu import -input=false -lock-timeout=5m \
  google_project.notifications "$app_project"
test "$(tofu state show -no-color google_project.notifications | \
  sed -n 's/^[[:space:]]*project_id[[:space:]]*=[[:space:]]*"\([^"]*\)"/\1/p')" = "$app_project"

gcloud billing projects link "$app_project" \
  --billing-account="$billing_account_id" \
  --billing-project="$quota_project"
app_billing=$(gcloud billing projects describe "$app_project" \
  --billing-project="$quota_project" --format=json) || exit 1
printf '%s' "$app_billing" | jq -e --arg account "$billing_account_id" '
  .billingEnabled == true and
  .billingAccountName == ("billingAccounts/" + $account)
' >/dev/null
unset app_billing
```

### 4. Inspect and remove the app default network

Google may create an auto-mode `default` network for a new project. OpenTofu's
project importer records `auto_create_network=true`, while provider 7.40.0 does
not remove an existing network when a later plan normalizes it to `false`.
The state project never needs Compute: enabling it could create a broad default
Compute service account in the project that holds state. Prove the API remains
disabled there. This does not claim that a dormant `default` network is absent;
without enabling Compute, the operator cannot safely inspect or remove it.
Enable Compute only in the isolated app project, import that API into state,
and inspect its network. The first reviewed plan deprivileges the app's default
service accounts after all required APIs are active.

```sh
state_compute=$(gcloud services list --enabled \
  --project="$state_project" --billing-project="$quota_project" \
  --filter='config.name=compute.googleapis.com' \
  --format='value(config.name)') || exit 1
test -z "$state_compute"
unset state_compute

gcloud services enable compute.googleapis.com \
  --project="$app_project" --billing-project="$quota_project"
tofu import -input=false -lock-timeout=5m \
  'google_project_service.required["compute.googleapis.com"]' \
  "$app_project/compute.googleapis.com"

attempt=1
while test "$attempt" -le 12; do
  if default_network=$(gcloud compute networks list \
      --project="$app_project" --billing-project="$quota_project" \
      --filter='name=default' --format='value(name)'); then
    case "$default_network" in
      default) network_status=present ;;
      "") network_status=absent ;;
      *) printf 'unexpected default-network result: %s\n' "$default_network" >&2; exit 1 ;;
    esac
  else
    network_status=query-error
  fi
  printf 'poll: project=%s default-network=%s attempt=%s/12\n' \
    "$app_project" "$network_status" "$attempt"
  test "$network_status" = present && break
  sleep 5
  attempt=$((attempt + 1))
done
test "${network_status:-}" = present

expected_rules='default-allow-icmp
default-allow-internal
default-allow-rdp
default-allow-ssh'
actual_rules=$(gcloud compute firewall-rules list \
  --project="$app_project" --billing-project="$quota_project" \
  --filter="network~'/global/networks/default$'" \
  --format='value(name)' | LC_ALL=C sort)
test "$actual_rules" = "$expected_rules"
gcloud compute networks describe default \
  --project="$app_project" --billing-project="$quota_project" \
  --format='yaml(name,autoCreateSubnetworks,routingConfig,subnetworks)'
gcloud compute networks subnets list \
  --project="$app_project" --billing-project="$quota_project" \
  --filter="network~'/global/networks/default$'"
gcloud compute routes list \
  --project="$app_project" --billing-project="$quota_project" \
  --filter="network~'/global/networks/default$'"

compute_account="${app_project_number}-compute@developer.gserviceaccount.com"
attempt=1
while test "$attempt" -le 24; do
  if gcloud iam service-accounts describe "$compute_account" \
      --project="$app_project" --billing-project="$quota_project" \
      --format='value(email)' >/dev/null 2>&1; then
    compute_account_status=present
  else
    compute_account_status=not-ready
  fi
  printf 'poll: project=%s compute-default-account=%s attempt=%s/24\n' \
    "$app_project" "$compute_account_status" "$attempt"
  test "$compute_account_status" = present && break
  sleep 5
  attempt=$((attempt + 1))
done
test "${compute_account_status:-}" = present

compute_member="serviceAccount:$compute_account"
attempt=1
while test "$attempt" -le 24; do
  app_policy=$(gcloud projects get-iam-policy "$app_project" \
    --billing-project="$quota_project" --format=json) || exit 1
  compute_roles=$(printf '%s' "$app_policy" | jq -r --arg member "$compute_member" '
    [.bindings[] | select((.members // []) | index($member)) | .role] |
    sort | .[]
  ')
  case "$compute_roles" in
    roles/editor) compute_role_status=ready ;;
    "") compute_role_status=not-ready ;;
    *) printf 'unexpected default Compute account roles: %s\n' "$compute_roles" >&2; exit 1 ;;
  esac
  printf 'poll: project=%s compute-default-role=%s attempt=%s/24\n' \
    "$app_project" "$compute_role_status" "$attempt"
  test "$compute_role_status" = ready && break
  sleep 5
  attempt=$((attempt + 1))
done
test "${compute_role_status:-}" = ready
printf 'default Compute service-account roles: %s\n' "$compute_roles"
```

Stop and obtain explicit approval for the exact inventory before deletion. At
that later checkpoint, re-run the app-project inventory and require the same
four firewall rules. Delete only those rules and `default`. If any command
partially succeeds, inventory again and request approval for only the remaining
resources; do not replay the deletion block blindly. Never enable Compute in
the state project merely to inspect a network.

After that approval, execute only the reviewed deletions:

```sh
gcloud compute firewall-rules delete \
  default-allow-icmp \
  default-allow-internal \
  default-allow-rdp \
  default-allow-ssh \
  --project="$app_project" --billing-project="$quota_project" --quiet
gcloud compute networks delete default \
  --project="$app_project" --billing-project="$quota_project" --quiet
```

After the approved deletion, require a successful empty app-network query, the
managed app Compute API, and the still-disabled state Compute API:

```sh
default_network=$(gcloud compute networks list \
  --project="$app_project" --billing-project="$quota_project" \
  --filter='name=default' --format='value(name)') || exit 1
test -z "$default_network"
test "$(gcloud services list --enabled \
  --project="$app_project" --billing-project="$quota_project" \
  --filter='config.name=compute.googleapis.com' \
  --format='value(config.name)')" = compute.googleapis.com
state_compute=$(gcloud services list --enabled \
  --project="$state_project" --billing-project="$quota_project" \
  --filter='config.name=compute.googleapis.com' \
  --format='value(config.name)') || exit 1
test -z "$state_compute"
unset app_policy compute_account compute_account_status compute_member compute_role_status compute_roles
unset default_network network_status
unset actual_rules expected_rules state_compute
```

### 5. Review and apply the remaining graph

Use a mode-0600 saved plan. It may normalize each imported project's
`auto_create_network` value from `true` to `false` in place. It must not create,
replace, or delete either project or the state bucket. Review every IAM grant,
API, Firebase resource, key restriction, and Hosting resource. Run the source
scanner and the concrete-plan project IAM gate; both must exit zero. A plan file
is never a rollback artifact.

```sh
umask 077
git check-ignore -q reviewed.plan
git check-ignore -q .terraform/reviewed-plan.json
test ! -e reviewed.plan
test ! -L reviewed.plan
test ! -e .terraform/reviewed-plan.json
test ! -L .terraform/reviewed-plan.json
tofu plan -input=false -lock=true -lock-timeout=5m -out=reviewed.plan
test -f reviewed.plan
test ! -L reviewed.plan
test "$(stat -c '%a' reviewed.plan)" = 600
tofu show -no-color reviewed.plan
tofu show -json reviewed.plan > .terraform/reviewed-plan.json
test -f .terraform/reviewed-plan.json
test ! -L .terraform/reviewed-plan.json
test "$(stat -c '%a' .terraform/reviewed-plan.json)" = 600
trivy config --exit-code 1 --misconfig-scanners=terraform .
sh check-plan-iam.sh .terraform/reviewed-plan.json
```

The source scan is supplemental: unresolved variables and dynamic `for_each`
grants can look clean without having been checked. `check-plan-iam.sh` uses the
existing `jq` dependency to inspect resolved resources in the full saved plan,
including child modules. It allows only these reviewed project IAM combinations:

- `roles/firebasehosting.admin` and `roles/serviceusage.apiKeysViewer` for
  `louiselm-hosting-deployer@louiselm.iam.gserviceaccount.com` in `louiselm`, at
  the existing Hosting grant addresses.
- `projects/louiselm/roles/louiselmFcmSender` for
  `louiselm-fcm-sender@louiselm.iam.gserviceaccount.com`, at the existing sender
  grant address; the custom role may contain only `cloudmessaging.messages.create`.

Other project grants, authoritative bindings/policies, widened custom roles,
and unknown IAM values fail closed. The sender role name is derived from its
resource project and role ID so it is known before first apply. A failed gate
requires private plan inspection and a reviewed configuration/policy correction
followed by a fresh full plan, never an override or a targeted plan to evade it.
This is not a general cloud-security scanner: service-account/WIF policy,
non-project IAM, resource destruction, and live effective permissions still
require the native tests and the operator reviews/proofs in this runbook.
The checker prints no plan values. Keep both plan files private and out of CI
artifacts; do not paste their JSON into logs or issue comments.

Apply only that exact reviewed artifact after a separate explicit approval:

```sh
tofu apply reviewed.plan
```

After apply, prove project identity, Compute default-account deprivileging, the state
boundary, and idempotence. The app's `auto_create_network=false` state is a
configuration check; the successful empty network query is the live proof:

```sh
test "$(tofu state show -no-color google_project.notifications | \
  sed -n 's/^[[:space:]]*auto_create_network[[:space:]]*=[[:space:]]*//p')" = false

for project in "$state_project" "$app_project"; do
  project_json=$(gcloud projects describe "$project" \
    --billing-project="$quota_project" --format=json)
  printf '%s' "$project_json" | jq -e --arg id "$project" '
    .projectId == $id and
    .lifecycleState == "ACTIVE" and
    (.parent // null) == null
  ' >/dev/null
done
unset project project_json

default_network=$(gcloud compute networks list \
  --project="$app_project" --billing-project="$quota_project" \
  --filter='name=default' --format='value(name)') || exit 1
test -z "$default_network"
state_compute=$(gcloud services list --enabled \
  --project="$state_project" --billing-project="$quota_project" \
  --filter='config.name=compute.googleapis.com' \
  --format='value(config.name)') || exit 1
test -z "$state_compute"

state_policy=$(gcloud projects get-iam-policy "$state_project" \
  --billing-project="$quota_project" --format=json)
bucket_policy=$(gcloud storage buckets get-iam-policy "gs://$state_bucket" \
  --billing-project="$quota_project" --format=json)
managed_folders=$(gcloud storage managed-folders list "gs://$state_bucket/" \
  --billing-project="$quota_project" --uri) || exit 1
test -z "$managed_folders"
state_compute_member="serviceAccount:${state_project_number}-compute@developer.gserviceaccount.com"
printf '%s' "$state_policy" | jq -e --arg member "$state_compute_member" '
  [.bindings[].members[]?] | index($member) == null
' >/dev/null

wif_provider=$(tofu output -raw gitlab_workload_identity_provider)
wif_pool=${wif_provider%/providers/*}
test "$wif_pool" != "$wif_provider"
wif_member_prefix="principalSet://iam.googleapis.com/${wif_pool}/"
wif_principal_prefix="principal://iam.googleapis.com/${wif_pool}/"
app_principal_set_prefix="principalSet://cloudresourcemanager.googleapis.com/projects/${app_project_number}/"
for policy in "$state_policy" "$bucket_policy"; do
  printf '%s' "$policy" | jq -e \
    --arg wif_member "$wif_member_prefix" \
    --arg wif_principal "$wif_principal_prefix" \
    --arg app_set "$app_principal_set_prefix" \
    --arg app_id "$app_project" \
    --arg app_number "$app_project_number" '
      [.bindings[].members[]? |
        select(startswith($wif_member) or
          startswith($wif_principal) or
          startswith($app_set) or
          (startswith("serviceAccount:") and
            (endswith("@" + $app_id + ".iam.gserviceaccount.com") or
              contains($app_number) or
              . == "serviceAccount:" + $app_id + "@appspot.gserviceaccount.com")))] |
      length == 0
    ' >/dev/null
done

app_policy=$(gcloud projects get-iam-policy "$app_project" \
  --billing-project="$quota_project" --format=json)
managed_accounts=$(gcloud iam service-accounts list \
  --project="$app_project" --billing-project="$quota_project" \
  --format='value(email)')
policy_accounts=$(printf '%s' "$app_policy" | jq -r '
  [.bindings[].members[]? |
    select(startswith("serviceAccount:")) |
    ltrimstr("serviceAccount:")] |
  unique | .[]
')
compute_account="${app_project_number}-compute@developer.gserviceaccount.com"
app_principals=$(printf '%s\n%s\n' \
  "$managed_accounts" "$policy_accounts" |
  sed '/^$/d' | LC_ALL=C sort -u)
test -n "$app_principals"

compute_roles=$(printf '%s' "$app_policy" | jq -r \
  --arg member "serviceAccount:$compute_account" '
    [.bindings[] | select((.members // []) | index($member)) | .role] |
    sort | .[]
  ')
test -z "$compute_roles"

bucket_resource="//storage.googleapis.com/projects/_/buckets/$state_bucket"
state_project_resource="//cloudresourcemanager.googleapis.com/projects/$state_project"
while IFS= read -r principal; do
  for permission in \
    storage.objects.get \
    storage.objects.create \
    storage.objects.delete \
    storage.objects.list \
    storage.objects.update \
    storage.buckets.delete \
    storage.buckets.setIamPolicy \
    storage.buckets.update; do
    access=$(gcloud policy-intelligence troubleshoot-policy iam "$bucket_resource" \
      --principal-email="$principal" \
      --permission="$permission" \
      --project="$state_project" \
      --billing-project="$state_project" \
      --format='value(overallAccessState)')
    printf 'access-check: principal=%s permission=%s result=%s\n' \
      "$principal" "$permission" "$access"
    test "$access" = CANNOT_ACCESS
  done

  access=$(gcloud policy-intelligence troubleshoot-policy iam "$state_project_resource" \
    --principal-email="$principal" \
    --permission=resourcemanager.projects.setIamPolicy \
    --project="$state_project" \
    --billing-project="$state_project" \
    --format='value(overallAccessState)')
  printf 'access-check: principal=%s permission=%s result=%s\n' \
    "$principal" resourcemanager.projects.setIamPolicy "$access"
  test "$access" = CANNOT_ACCESS
done <<EOF
$app_principals
EOF
unset access app_policy app_principal_set_prefix app_principals bucket_policy
unset bucket_resource compute_account compute_roles
unset managed_accounts managed_folders permission policy
unset policy_accounts principal state_compute state_compute_member state_policy
unset state_project_resource wif_member_prefix wif_pool wif_principal_prefix
unset wif_provider

new_generation=$(gcloud storage objects describe \
  "gs://$state_bucket/production/default.tfstate" \
  --billing-project="$quota_project" --format='value(generation)')
test -n "$new_generation"
printf 'post-apply GCS state generation: %s\n' "$new_generation"
tofu plan -input=false -lock=true -lock-timeout=5m -detailed-exitcode
```

Policy Troubleshooter must return exactly `CANNOT_ACCESS`; unknown or partial
results fail acceptance. It does not evaluate Cloud Storage ACLs, so the
bucket's separately asserted uniform bucket-level access closes that path.
Audit logs are supplemental evidence with the platform's `_Default` retention,
not a substitute for object-version and soft-delete recovery.

`tofu plan -detailed-exitcode` must return 0. Exit 1 is an error; exit 2 is
drift or a non-idempotent configuration and blocks completion. Do not run
destroy: both projects, both liens, the bucket, and durable Firebase resources
are deletion-protected. Rollback is a new reviewed plan that retains both
projects and the state bucket.

After successful acceptance, remove the local sensitive plan artifacts:

```sh
find ./reviewed.plan ./.terraform/reviewed-plan.json -type f -delete
```

Once the app APIs exist, stop charging client-library quota to the bootstrap
project without persisting a new ADC default:

```sh
unset GOOGLE_CLOUD_QUOTA_PROJECT
unset billing_account_id
```

## Interrupted bootstrap and state recovery

If OpenTofu left `errored.tfstate`, preserve it before restarting the complete
preflight, whose first-bootstrap guard deliberately rejects that filename. Run
this from the clean checkout root; the temporary directory must remain private
until recovery is complete:

```bash
set +x
set -euo pipefail
errored_state_copy=
if test -e infra/firebase/errored.tfstate || test -L infra/firebase/errored.tfstate; then
  test -f infra/firebase/errored.tfstate
  test ! -L infra/firebase/errored.tfstate
  git check-ignore -q infra/firebase/errored.tfstate
  test "$(stat -c '%a' infra/firebase/errored.tfstate)" = 600
  recovery_evidence_dir=$(mktemp -d)
  test "$(stat -c '%a' "$recovery_evidence_dir")" = 700
  errored_state_copy="$recovery_evidence_dir/errored.tfstate"
  mv -- infra/firebase/errored.tfstate "$errored_state_copy"
  test -f "$errored_state_copy"
fi
```

Now restart with the complete preflight in the same shell and a new isolated
`TF_DATA_DIR`, then re-establish the four fixed shell variables and billing
account lookup. Re-run both exact project-identity checks so
`state_project_number` and `app_project_number` come from successful live
describes rather than the interrupted shell. Once the bucket's exact ownership
and protections are proven, run `tofu init -reconfigure`; do not reuse cached
backend metadata.

Inspect before acting in this order: exact project identity and lifecycle,
billing link, enabled APIs, bucket ownership/security metadata, then GCS state
object existence. Check live, noncurrent, and soft-deleted generations before
calling state absent. Never infer absence from a failed `describe` request. If
either project is absent, follow its matching first-bootstrap create step once
and restart the recovery inspection. After both projects are proven to exist,
rerun their exact JSON assertions and derive both numbers again from the
successful responses:

```sh
state_project_json=$(gcloud projects describe "$state_project" \
  --billing-project="$quota_project" --format=json) || exit 1
app_project_json=$(gcloud projects describe "$app_project" \
  --billing-project="$quota_project" --format=json) || exit 1
printf '%s' "$state_project_json" | jq -e --arg id "$state_project" '
  .projectId == $id and .name == "LouiseLM State" and
  .lifecycleState == "ACTIVE" and (.parent // null) == null
' >/dev/null
printf '%s' "$app_project_json" | jq -e --arg id "$app_project" '
  .projectId == $id and .name == "LouiseLM" and
  .lifecycleState == "ACTIVE" and (.parent // null) == null
' >/dev/null
state_project_number=$(printf '%s' "$state_project_json" | jq -er .projectNumber)
app_project_number=$(printf '%s' "$app_project_json" | jq -er .projectNumber)
unset app_project_json state_project_json
```

- If the live bucket is absent, inspect soft-deleted buckets before any create.
  The pinned gcloud 582.0.0 reports the exact no-match error below when there
  are none; any other error is a stop condition. If the exact bucket has a
  soft-deleted generation, require separate approval to restore
  `gs://louiselm-tfstate#GENERATION`, then re-prove every bucket property. Only
  when both live and soft-deleted inventories are empty and retained evidence
  shows the original create never succeeded may the original create step run
  once.
- If the GCS state object is absent, initialize and import the exact state
  project, four services, and bucket. `tofu state list` normally fails before
  the first snapshot exists; do not treat that failure as an empty state.
- If the state object exists, initialize the backend, run `tofu state list`,
  and import only missing addresses. Never use `-migrate-state` or
  `-force-copy`; the former GitLab state was empty and remains a separate
  recovery clue until GCS acceptance completes.
- If `louiselm` is `ACTIVE` but absent from state, import
  `google_project.notifications` before any other mutation. Never rerun project
  creation. If Compute is already enabled but its service address is absent,
  import `google_project_service.required["compute.googleapis.com"]` before the
  next plan.
- If either project is `DELETE_REQUESTED`, stop for explicit restore and billing
  review. Project IDs cannot be recycled.
- If `errored_state_copy` is nonempty, compare only its `lineage` and `serial`
  with `tofu state pull` before proposing a reviewed recovery; never discard it
  or push it blindly.

Inspect soft-deleted buckets without accepting an arbitrary CLI failure as
absence:

```sh
soft_bucket_error="$TF_DATA_DIR/soft-buckets.err"
if soft_buckets=$(gcloud storage ls --buckets --soft-deleted --json \
    --exhaustive --project="$state_project" --billing-project="$quota_project" \
    2>"$soft_bucket_error"); then
  :
else
  test -z "$soft_buckets"
  grep -Fxq \
    'ERROR: (gcloud.storage.ls) One or more URLs matched no objects.' \
    "$soft_bucket_error"
  soft_buckets='[]'
fi
printf '%s' "$soft_buckets" | jq -e --arg bucket "$state_bucket" '
  all(.url | startswith("gs://" + $bucket + "#"))
' >/dev/null
printf '%s' "$soft_buckets" | jq -r '.[].url'
rm -f -- "$soft_bucket_error"
unset soft_bucket_error soft_buckets
```

After selecting and separately approving an exact soft-deleted bucket
generation, restore only that generation, then rerun the full bucket JSON
assertion from the bootstrap before inspecting any objects:

```sh
gcloud storage restore \
  "gs://$state_bucket#REVIEWED_GENERATION" \
  --billing-project="$quota_project"
```

GCS object versioning and soft deletion are the recovery sources. List and
validate both classes as metadata without printing state. Pinned gcloud 582.0.0
uses `LIVE_AND_NONCURRENT` for the default `objects list` view. Unlike `gcloud
storage ls`, it returns a successful empty JSON array for an absent exact object
while auth and API errors remain fatal:

```sh
state_object=production/default.tfstate
live_noncurrent_json=$(gcloud storage objects list \
  "gs://$state_bucket/$state_object" \
  --billing-project="$quota_project" --raw --format=json) || exit 1
printf '%s' "$live_noncurrent_json" | jq -e \
  --arg bucket "$state_bucket" --arg name "$state_object" '
    type == "array" and
    all(.[];
      .bucket == $bucket and .name == $name and
      (.generation | type == "string") and
      (.generation | test("^[0-9]+$")) and
      ((.timeDeleted // null) == null or (.timeDeleted | type == "string"))) and
    ([.[] | select((.timeDeleted // null) == null)] | length) <= 1 and
    ([.[].generation] | unique | length) == length
  ' >/dev/null

soft_deleted_json=$(gcloud storage objects list \
  "gs://$state_bucket/$state_object" \
  --soft-deleted --exhaustive \
  --billing-project="$quota_project" --raw --format=json) || exit 1
printf '%s' "$soft_deleted_json" | jq -e \
  --arg bucket "$state_bucket" --arg name "$state_object" '
    type == "array" and
    all(.[];
      .bucket == $bucket and .name == $name and
      (.generation | type == "string") and
      (.generation | test("^[0-9]+$")) and
      (.softDeleteTime | type == "string") and
      (.hardDeleteTime | type == "string")) and
    ([.[].generation] | unique | length) == length
  ' >/dev/null

inventory=$(jq -n \
  --argjson current "$live_noncurrent_json" \
  --argjson soft "$soft_deleted_json" '
    {
      live: [$current[] | select((.timeDeleted // null) == null) |
        {generation, size, timeCreated, updated}],
      noncurrent: [$current[] | select((.timeDeleted // null) != null) |
        {generation, size, timeCreated, timeDeleted, updated}],
      soft_deleted: [$soft[] |
        {generation, size, timeCreated, softDeleteTime, hardDeleteTime}]
    }
  ')
printf '%s\n' "$inventory" | jq .
unset live_noncurrent_json soft_deleted_json
```

If the reviewed generation is soft-deleted, require a separate approval and
restore it only while no live object exists. Generation-match zero prevents an
overwrite. First prove that no live backend lock exists. Replace
`REVIEWED_GENERATION` with the selected numeric generation; a successful
restore creates a new live generation, which must become the download target:

```sh
lock_object=production/default.tflock
lock_inventory=$(gcloud storage objects list \
  "gs://$state_bucket/$lock_object" \
  --billing-project="$quota_project" --raw --format=json) || exit 1
printf '%s' "$lock_inventory" | jq -e \
  --arg bucket "$state_bucket" --arg name "$lock_object" '
    type == "array" and
    all(.[]; .bucket == $bucket and .name == $name) and
    ([.[] | select((.timeDeleted // null) == null)] | length) == 0
  ' >/dev/null
unset lock_inventory lock_object

download_generation=REVIEWED_GENERATION
gcloud storage restore \
  "gs://$state_bucket/production/default.tfstate#$download_generation" \
  --if-generation-match=0 \
  --billing-project="$quota_project"
download_generation=$(gcloud storage objects describe \
  "gs://$state_bucket/production/default.tfstate" \
  --billing-project="$quota_project" --format='value(generation)') || exit 1
test -n "$download_generation"
printf 'restored live generation: %s\n' "$download_generation"
```

Download the reviewed live or noncurrent generation to a new ignored,
mode-0600 file. For a live or noncurrent generation that did not need restore,
set `download_generation=REVIEWED_GENERATION` first:

```sh
umask 077
git check-ignore -q recovered.tfstate
test ! -e recovered.tfstate
test ! -L recovered.tfstate
gcloud storage cp \
  "gs://$state_bucket/production/default.tfstate#$download_generation" \
  recovered.tfstate \
  --no-clobber \
  --billing-project="$quota_project"
test -f recovered.tfstate
test ! -L recovered.tfstate
test "$(stat -c '%a' recovered.tfstate)" = 600
jq '{lineage, serial}' recovered.tfstate
tofu state pull | jq '{lineage, serial}'
if test -n "${errored_state_copy:-}"; then
  jq '{lineage, serial}' "$errored_state_copy"
fi
```

Do not run `tofu state push` until the selected generation, current lineage,
serial, current lock holder, and all intervening changes have been reviewed and
the overwrite has separate explicit approval. Immediately before that approved
push, rerun the live `default.tflock` absence assertion above. Remove
`recovered.tfstate` and the private recovery-evidence directory after the
approved recovery and verification; they are sensitive, not archives.

## Protected GitLab CI

`.gitlab-ci.yml` pins the to-be-continuous Terraform 9.4.0 source to immutable
commit `fef22b89d78f356aeac7b7357fd8e5984d05bbca`. It uses the component only for
credential-free formatting, validation, linting, and source scanning; GitLab
backend integration and infrastructure plan/apply jobs are disabled. A
standalone backend-free job runs `sh tests/test-plan-iam.sh`: the native OpenTofu
suite, a plan produced by its mocked providers through the production IAM gate,
and negative fixtures for dynamic privileged grants and malformed/unknown input.
It uses `jq`, already required by the operator runbook, and receives neither
cloud credentials nor a real plan. This tests the gate; it does not approve the
operator's production plan. Branches and merge requests also build and inspect
the public site without an ID token.

After the reviewed operator apply creates WIF and Hosting, configure only these
protected GitLab variables:

```text
GCP_OIDC_PROVIDER = tofu output -raw gitlab_workload_identity_provider
GCP_HOSTING_OIDC_ACCOUNT = tofu output -raw hosting_deployer_service_account_email
TF_VAR_project_id = louiselm
```

Cancel older pending/manual production jobs before setting protected
`LOUISELM_PRODUCTION_READY=true`. GitLab evaluates rules when creating a
pipeline, so changing the variable does not revoke a job in an existing
pipeline. Scope `GCP_HOSTING_OIDC_ACCOUNT` to `site-production` if supported.

The WIF condition accepts only project 163's protected `main` branch with the
`site-production` environment claim. It may impersonate only the Hosting
deployer, which has Firebase's documented `roles/firebasehosting.admin` and
`roles/serviceusage.apiKeysViewer` pair. It has no state-project role and
cannot administer project IAM or FCM. Do not add a Google key or an
infrastructure CI identity.

The manual `site-deploy` job deploys only the checked `_build/html` artifact and
retains `firebase-deploy.json` with the immutable Hosting version. To restore a
known-good retained version under the same short-lived ADC setup:

```sh
version_name=$(node -p 'require("./firebase-deploy.json").result.hosting')
version_id=${version_name##*/}
site_id=$TF_VAR_project_id
firebase hosting:clone "${site_id}@${version_id}" "${site_id}:live" \
  --project "$TF_VAR_project_id" --non-interactive
```

This creates a new Hosting release and does not change OpenTofu state.

## Installing Android configuration

After the reviewed apply, write the sensitive Firebase output directly to a
protected local file:

```sh
umask 077
git check-ignore -q google-services.json
test ! -e google-services.json
test ! -L google-services.json
tofu output -raw android_firebase_config_json > google-services.json
test -f google-services.json
test ! -L google-services.json
test "$(stat -c '%a' google-services.json)" = 600
```

Install runtime sender credentials through the deployment secret manager or a
workload identity. This composition never creates a private key, so a key
cannot enter OpenTofu state accidentally.
