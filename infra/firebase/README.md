# LouiseLM Firebase foundation

This composition creates a dedicated, deletion-protected Google Cloud project,
enables the APIs needed by Firebase Cloud Messaging, registers the Android
application, and creates a restricted Android API key. It also creates the
public Firebase Hosting site, a Hosting-only GitLab deployer, and a runtime
sender service account bound to a custom role containing only
`cloudmessaging.messages.create`. Hosting releases and website files remain CI
artifacts rather than OpenTofu resources.

The composition deliberately creates no service-account private key, FCM server
secret, device token, or generated Android config file. The Firebase config is
available only as a sensitive output so an operator can install it out of band.

## Inputs and public-repository boundary

Copy `terraform.tfvars.example` to a local, ignored `terraform.tfvars` and fill
in the dedicated project ID, billing account, and Android signing certificate
fingerprint. An organization or folder parent is optional. Never commit that
file, state, plans, credentials, or `google-services.json`. The root declares
an unconfigured HTTP backend; GitLab CI supplies its address, locking endpoints,
and short-lived credentials through `TF_HTTP_*` environment variables.

The project may be parentless. If assigning a parent, set at most one of
`org_id` or `folder_id`. `billing_account_id` is mandatory. The project,
Firebase project, Android app, audit policy, and enabled APIs use deletion
protection. Project-wide Data Access audit logging improves traceability but
can add Cloud Logging volume and cost; review actual usage after bootstrap
before proposing any exclusion.

## Reviewable workflow

Use OpenTofu 1.12 or newer and the pinned providers from the committed lock
file:

Backend-free validation needs no credentials:

```sh
tofu fmt -check -recursive
tofu init -backend=false
tofu validate
```

Before any local plan or apply, configure `TF_HTTP_*` for the same `production`
GitLab state used by CI. If a local `terraform.tfstate` exists, back it up outside
the repository and use `tofu init -migrate-state`; never discard it with
`-reconfigure`. In a clean checkout with no local state, initialize and save a
reviewable plan:

```sh
tofu init -reconfigure
tofu plan -out=notifications.plan
tofu show notifications.plan
```

Inspect the saved plan for project/API deletion, Android key restrictions,
custom IAM permissions, Hosting site replacement, and replacement of the
project or Android app. Apply only that reviewed artifact:

```sh
tofu apply notifications.plan
```

Never use `tofu apply` without a reviewed saved plan for this composition. Do
not run destroy: the project is deletion-protected. Rollback is a reviewed plan
that removes the sender IAM binding or disables an API while retaining the
project and its data.

## Protected GitLab CI

`.gitlab-ci.yml` includes to-be-continuous Terraform and its Google Cloud
variant at `9.4.0`. Production plan and apply run only on protected `main`, use
GitLab-managed HTTP state, and receive a short-lived OIDC token only in those
jobs. The Developer-only plan artifact expires after one day and includes the
binary plan consumed by the blocking manual apply. Branches and merge requests
also build and inspect the public site without an ID token. The component's
pre-init hook runs `tf-pre-init.sh`, which rejects the configuration if its
required HTTP backend declaration is removed.

The first bootstrap is local: fill the GitLab inputs, authenticate with
operator ADC, and apply one reviewed saved plan. Then configure these protected
GitLab variables from outputs without printing sensitive values:

```text
GCP_OIDC_PROVIDER = tofu output -raw gitlab_workload_identity_provider
GCP_OIDC_ACCOUNT  = tofu output -raw ci_service_account_email
GCP_HOSTING_OIDC_ACCOUNT = tofu output -raw hosting_deployer_service_account_email
```

Set the remaining `LOUISELM_*`, `GITLAB_OIDC_ISSUER_URL`, `GITLAB_WIF_POOL_ID`,
and `GITLAB_WIF_PROVIDER_ID` variables as protected project variables, along
with the `TF_VAR_*` infrastructure inputs. Omit both parent variables for a
parentless project; otherwise set at most one of protected `TF_VAR_org_id` and
`TF_VAR_folder_id`. Scope `GCP_HOSTING_OIDC_ACCOUNT` to the `site-production`
environment when the GitLab tier supports environment-scoped variables. Do not
add a Google key: production jobs write a short-lived external-account ADC file
from their job ID token. The `production` environment may impersonate only the
infrastructure account; `site-production` may impersonate only the Hosting
deployer. Never commit state, plans, credentials, Android configuration, or a
Firebase CLI token.

The Hosting deployer receives Firebase's supported predefined deployment pair:
`roles/firebasehosting.admin` and `roles/serviceusage.apiKeysViewer`. It cannot
administer project IAM or FCM, but Hosting Admin covers every Hosting site in
this project and API Keys Viewer can read the restricted Android client key.

For federation failures, verify the issuer's trailing slash, the job audience
without a trailing slash, the numeric project ID, protected branch, GitLab
environment claim, and `roles/iam.workloadIdentityUser` binding. Recovery
disables the CI variables and uses operator ADC for a reviewed plan. Rollback
is a reviewed exact plan; never destroy the deletion-protected project.

After the Hosting site and deployer exist, a protected-main pipeline exposes a
manual `site-deploy` job. It deploys only the checked `_build/html` artifact and
retains `firebase-deploy.json`, containing the immutable Hosting version name,
for one year. To restore a retained known-good version under the same
short-lived ADC setup:

```sh
version_name=$(node -p 'require("./firebase-deploy.json").result.hosting')
version_id=${version_name##*/}
site_id=$TF_VAR_project_id
firebase hosting:clone "${site_id}@${version_id}" "${site_id}:live" \
  --project "$TF_VAR_project_id" --non-interactive
```

Use the artifact from the last known-good pipeline. This creates a new Hosting
release and does not change OpenTofu state.

## Installing Android configuration

After a reviewed apply, retrieve the sensitive output directly into a protected
local file; do not print it in CI logs or commit it:

```sh
umask 077
tofu output -raw android_firebase_config_json > google-services.json
```

Install or rotate the runtime sender credentials out of band through the
deployment secret manager or workload identity. This composition does not
create a private key, so key rotation cannot accidentally enter Terraform
state.
