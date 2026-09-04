# LouiseLM Firebase notification foundation

This composition creates a dedicated, deletion-protected Google Cloud project,
enables the APIs needed by Firebase Cloud Messaging, registers the Android
application, and creates a restricted Android API key. It also creates a
runtime sender service account bound to a custom role containing only
`cloudmessaging.messages.create`.

The composition deliberately creates no service-account private key, FCM server
secret, device token, or generated Android config file. The Firebase config is
available only as a sensitive output so an operator can install it out of band.

## Inputs and public-repository boundary

Copy `terraform.tfvars.example` to a local, ignored `terraform.tfvars` and fill
in the dedicated project ID, billing account, organization or folder parent,
and Android signing certificate fingerprint. Never commit that file, state,
plans, credentials, or `google-services.json`. The root declares an
unconfigured HTTP backend; GitLab CI supplies its address, locking endpoints,
and short-lived credentials through `TF_HTTP_*` environment variables.

The project parent is intentionally explicit: exactly one of `org_id` or
`folder_id` is required. `billing_account_id` is also mandatory. The project,
Firebase project, Android app, and enabled APIs use deletion protection.

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
custom IAM permissions, and replacement of the project or Android app. Apply
only that reviewed artifact:

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
binary plan consumed by the blocking manual apply. Public branches and merge
requests run only formatting and backend-free validation. The component's
pre-init hook runs `tf-pre-init.sh`, which rejects the configuration if its
required HTTP backend declaration is removed.

The first bootstrap is local: fill the GitLab inputs, authenticate with
operator ADC, and apply one reviewed saved plan. Then configure these protected
GitLab variables from outputs without printing sensitive values:

```text
GCP_OIDC_PROVIDER = tofu output -raw gitlab_workload_identity_provider
GCP_OIDC_ACCOUNT  = tofu output -raw ci_service_account_email
```

Set the remaining `LOUISELM_*`, `GITLAB_OIDC_ISSUER_URL`, `GITLAB_WIF_POOL_ID`,
and `GITLAB_WIF_PROVIDER_ID` variables as protected project variables, along
with the `TF_VAR_*` infrastructure inputs (set exactly one of the protected
`TF_VAR_org_id` and `TF_VAR_folder_id` variables). Do not add a Google key: the
Google variant writes a short-lived external-account ADC file from the job ID
token. Never commit state, plans, credentials, or Android configuration.

For federation failures, verify the issuer's trailing slash, the job audience
without a trailing slash, the numeric project ID, protected branch, and
`roles/iam.workloadIdentityUser` binding. Recovery disables the CI variables
and uses operator ADC for a reviewed plan. Rollback is a reviewed exact plan;
never destroy the deletion-protected project.

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
