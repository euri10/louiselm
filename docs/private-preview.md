# Optional private previews for unmanaged Sessions

This recipe lets an operator review two dev servers over an existing SSH
connection. Each server stays on the development host's loopback interface;
the browser connects to a separate loopback port on the review device. It uses
existing tools and does not configure LouiseLM, a firewall, or an overlay.
Unmanaged Sessions are not Contained Sessions and this recipe establishes no
Verified posture. Do not use it to bypass a Contained Session's network boundary.

## Before starting

- Use a development host you control, an existing SSH account that permits
  local forwarding, and a review device you trust. Verify the SSH host key
  through an independent trusted channel and use a key already authorized on
  the host. SSH authenticates the host and the account; the account may have
  additional authority beyond these previews.
- Review the server and its configuration before exposing it. Use synthetic
  data and test credentials only. Dev servers can expose source files,
  environment values, APIs, logs, and write operations; exclude secrets and
  production credentials from both the served content and its dependencies.
  Do not put credentials, SSH keys, tokens, or private URLs in the command line,
  logs, issue records, screenshots, or browser history. A key's file path is
  needed by SSH; the private key's contents are not.
- Start each server in its own Session's worktree, with a distinct unused
  loopback port. Verify the actual listener, including any separate API or
  hot-reload listener. Do not bind to `0.0.0.0`, a LAN address, or a public
  interface. Avoid changing host or origin checks to allow arbitrary origins.
  On Linux, `ss -ltnp` shows listeners; inspect the relevant ports and their
  owners before proceeding. A port already in use belongs to its existing
  owner: choose another port rather than stopping it.

For example, if both projects already have a Vite `dev` script, run these in
separate terminals on the development host, in the corresponding worktrees:

```sh
# Session A
npm run dev -- --host 127.0.0.1 --port 5173 --strictPort

# Session B
npm run dev -- --host 127.0.0.1 --port 5174 --strictPort
```

These flags are specific to [Vite](https://vite.dev/guide/cli); use the
equivalent settings for another server. `--strictPort` makes a collision fail
instead of silently selecting a different port. Verify the listeners and what
each server actually serves. A worktree is an organizational aid, not a
security boundary.

## Forward and review

On the review device, replace the key path and host with the already authorized
key and the development host's real DNS name or address. This example uses
OpenSSH defaults without user configuration (`-F /dev/null`), so an SSH alias,
jump host, custom known-hosts file, or other setup requires an equivalent
explicit configuration. The server must permit local TCP forwarding.

```sh
ssh -F /dev/null -N -S none \
  -o StrictHostKeyChecking=yes -o IdentitiesOnly=yes \
  -o ForwardAgent=no -o ExitOnForwardFailure=yes \
  -i /path/to/authorized-ssh-key \
  -L 127.0.0.1:13101:127.0.0.1:5173 \
  -L 127.0.0.1:13102:127.0.0.1:5174 \
  user@dev-host
```

Keep this foreground SSH process running. `-S none` avoids reusing a shared
connection; `ExitOnForwardFailure` refuses a failed local bind, but cannot prove
that the remote server is reachable. Confirm the review-device listeners are
`127.0.0.1:13101` and `127.0.0.1:13102`, then visit both addresses:

| Review-device URL | Development-host destination |
| --- | --- |
| `http://127.0.0.1:13101/` | Session A: `127.0.0.1:5173` |
| `http://127.0.0.1:13102/` | Session B: `127.0.0.1:5174` |

Confirm each page is the intended worktree, including navigation, API calls,
and hot reload where needed. Absolute URLs, callbacks, and WebSocket ports may
need the application's own configuration; do not open another listener merely
to make them work. If the tunnel fails or either destination is wrong, stop and
correct the mapping before reviewing. See [OpenSSH forwarding](https://man.openbsd.org/ssh.1#L)
and [forward failure behavior](https://man.openbsd.org/ssh_config#ExitOnForwardFailure).

The SSH channel is encrypted and its account authenticates when the connection
is established. The two local HTTP listeners have **no additional login**:
other processes on the review device can connect to them, and processes sharing
the development host's network namespace may reach its loopback servers. SSH
does not make a dev server safe to expose to those processes or to browser
content: cross-origin requests can reach local services even when the browser
blocks reading the response. Use only trusted devices, code, and data; a tunnel
is not a sandbox.

Browsers generally treat top-level loopback HTTP URLs as potentially trustworthy
[secure contexts](https://developer.mozilla.org/en-US/docs/Web/Security/Defenses/Secure_Contexts).
Check `window.isSecureContext` in the actual browser; individual APIs, embedded
pages, cookies, and OAuth callbacks can impose additional requirements. The
loopback connection itself is HTTP, and a LAN or overlay IP over HTTP is not
equivalent to loopback or HTTPS. Different ports are different origins, but
cookies can still be shared across ports on a host. Use a separate, disposable
browser profile and avoid real credentials.

To review on a phone using this recipe, the phone must run the SSH client and
its browser must connect to the phone's own forwarded loopback ports. A tunnel
running only on a laptop does not make `127.0.0.1` on the phone reach the laptop.
Phone SSH clients and background behavior vary; verify the two pages and secure
context on the actual phone and network. This recipe has not been accepted on a
real phone or a real tailnet.

## Stop and verify cleanup

Stop the SSH process with Ctrl-C on the review device, then stop each of the two
server processes in its own terminal on the development host. Recheck the four
ports with the host's listener-inspection tool and confirm the owned listeners
are gone; do not kill an unrelated process if a port has been reused. Close the
disposable browser profile and remove any task-owned temporary data. If a
server or proxy was started separately, stopping SSH alone does not stop it.

## If using an existing private overlay or proxy instead

[Portless and Tailscale](https://domenic.me/agentic-coding-setup/) are another
way to route previews, but HTTPS alone does not decide who can reach them.
Before using an already provisioned proxy, identify its exact listener, route,
upstream, authenticated principals, and whether it remains active after the
terminal exits or the host restarts. Review effective tailnet grants/ACLs for
the exact source devices and destination ports; verify access from an authorized
device and denial from an unauthorized one. Tailscale's
[default policy can allow all tailnet devices](https://tailscale.com/docs/reference/examples/acls#allow-all-default-acl).
Its [Serve](https://tailscale.com/docs/features/tailscale-serve) feature is
subject to that policy; Funnel is public and is outside this recipe. Policy
does not restrict independent LAN access. If the existing policy or proxy
cannot provide the intended boundary, do not expose the preview.

Tailscale HTTPS certificates disclose the device and tailnet DNS names in
[public Certificate Transparency logs](https://tailscale.com/docs/how-to/set-up-https-certificates#machine-names-in-the-public-ledger).
Do not use sensitive names. Review certificate trust and hostname validation on
the actual browser; do not bypass certificate warnings. The operator owns any
overlay, proxy, policy, certificates, and removal of the exact routes they
started. LouiseLM neither configures nor verifies them here.
