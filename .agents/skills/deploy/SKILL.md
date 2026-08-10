---
name: deploy
description: Deploy or update Agent Note on ms04 by building the image locally, copying it into ms04's rootful Podman image store, publishing the same image to GHCR, and restarting only podman-agent-note.service. Use when asked to deploy Agent Note, update or publish its image, restart it on ms04, change its Nix-managed configuration, or verify agent-note.gsmlg.net.
---

# Deploy Agent Note

## Scope

Deploy this repository's application to `ms04`.

- NixOS configuration source: `https://git.gsmlg.net/gsmlg/nix-config.git`
- Host configuration: `hosts/gpu/ms04/apps.nix`
- Shared module: `modules/nixos/apps/agent-note/`
- Image name: `ghcr.io/gsmlg-dev/agent-note:latest`
- Service: `podman-agent-note.service`
- Container: `agent-note`
- Host endpoint: `http://127.0.0.1:16222/`
- Public endpoint: `https://agent-note.gsmlg.net/`

Build application images on the local workstation and copy them to `ms04`. Do not
copy source to `ms04`, build there, use Docker Compose, or edit the Nix-generated
systemd unit by hand. Image deployment does not require publishing to or pulling from
a container registry for the transfer itself. Keep nix-config's
`pullPolicy = "always"`: after copying the image to `ms04`, publish that same image to
GHCR before restarting so the Nix-generated service pulls the same artifact rather
than replacing it with an older registry image.

## Choose the deployment path

### Build and deploy the current working tree

Inspect the local tree and preserve all existing changes. The image build uses the
current working tree, including untracked files that are not excluded by
`.dockerignore`.

```sh
git status --short --branch
git rev-parse HEAD
```

Confirm that the local builder and `ms04` target architectures agree, then build the
image locally. On the current x86_64 hosts, the normal build is:

```sh
docker buildx build --progress=plain \
  --build-arg HTTP_PROXY="$HTTP_PROXY" \
  --build-arg HTTPS_PROXY="$HTTPS_PROXY" \
  --build-arg NO_PROXY="$NO_PROXY" \
  --load \
  --tag ghcr.io/gsmlg-dev/agent-note:latest \
  .
```

Forwarding the proxy variables as build arguments helps the builder fetch external
artifacts; they are not baked into the runtime image. Do not use `--push` during the
build: copy and inspect the locally built artifact before publishing it.

Prove that the local tag exists before transfer:

```sh
docker image inspect ghcr.io/gsmlg-dev/agent-note:latest \
  --format 'id={{.Id}} created={{.Created}}'
```

Copy the built image directly into the rootful Podman store on `ms04`:

```sh
set -o pipefail
docker save ghcr.io/gsmlg-dev/agent-note:latest \
  | gzip -1 \
  | ssh ms04 'gzip -dc | sudo podman load'
```

Do not publish or restart the service if the build, stream transfer, or `podman load`
fails.

### Update only the image

When the requested image already exists in the local Docker image store, skip the
build. Run only the image inspection, `docker save`/`podman load` transfer, Agent Note
publication, restart, and verification. Do not change nix-config or run a NixOS
rebuild for an image-only update.

### Publish the staged image

After the image has loaded successfully on `ms04`, push the exact same local tag to
GHCR and inspect the published manifest:

```sh
docker push ghcr.io/gsmlg-dev/agent-note:latest
docker buildx imagetools inspect ghcr.io/gsmlg-dev/agent-note:latest
```

Authenticate with `docker login ghcr.io` if needed, without printing or storing the
token in the repository. Do not rebuild or retag between copying and publication.
Because the Nix-managed service has `pullPolicy = "always"`, do not restart until both
the push and registry inspection succeed.

### Restart the service

After GHCR contains the copied image, restart only the Agent Note service with:

```sh
ssh ms04 'sudo systemctl restart podman-agent-note.service'
```

Do not restart PostgreSQL, TEI, Caddy, other Podman units, or the host.

### Update declarative service configuration

When ports, mounts, database, embedding service, hostname, pull policy, dependencies,
or other service settings change, update nix-config instead of mutating `ms04`
directly.

1. Use a checkout whose `origin` is
   `https://git.gsmlg.net/gsmlg/nix-config.git` and inspect its status before editing.
2. Make the smallest change in `hosts/gpu/ms04/apps.nix` or
   `modules/nixos/apps/agent-note/`.
3. Format and validate the affected Nix configuration according to that repository's
   instructions.
4. Commit and push only when the user requested or approved publication.
5. After the intended commit is available to `ms04`, apply it through the Nix-managed
   updater:

```sh
ssh ms04 'sudo systemctl start system-update.service'
```

The updater pulls the configured `/etc/nixos` checkout and switches
`/etc/nixos#ms04`. Do not run it before the intended configuration commit is
available remotely.

## Verification

First prove that the copied image is the one selected by the mutable local tag. After
restart, compare it with the running container's image ID:

```sh
ssh ms04 \
  'set -e;
   wanted=$(sudo podman image inspect \
     ghcr.io/gsmlg-dev/agent-note:latest --format "{{.Id}}");
   running=$(sudo podman inspect agent-note --format "{{.Image}}");
   printf "wanted=%s\nrunning=%s\n" "$wanted" "$running";
   test "$wanted" = "$running"'
```

Verify the unit, container, logs, and bounded HTTP readiness:

```sh
ssh ms04 \
  'set -e;
   systemctl is-active podman-agent-note.service;
   sudo podman ps --filter name="^agent-note$" \
     --format "{{.Names}} {{.Image}} {{.Status}} {{.Ports}}";
   sudo journalctl -u podman-agent-note.service -n 120 --no-pager;
   ready=0;
   for attempt in $(seq 1 30); do
     if curl -fsS -o /dev/null http://127.0.0.1:16222/; then
       ready=1;
       break;
     fi;
     sleep 1;
   done;
   test "$ready" -eq 1'

curl -fsS -I https://agent-note.gsmlg.net/
```

Report local build, image transfer/load, GHCR publication, service restart or NixOS
switch, image-ID match, and HTTP readiness as separate outcomes. A successful image
load is not a successful restart, and an active unit is not sufficient without an HTTP
response.

## Failure handling

- If the local build fails, do not transfer an older tag by mistake. Report the failed
  build and stop.
- If transfer or `podman load` fails, leave the old service running and do not publish
  or restart.
- If GHCR push or registry inspection fails, leave the old service running and do not
  restart; with `pullPolicy = "always"`, restarting before publication can run the old
  registry image.
- If restart succeeds but the first HTTP probe fails, allow the bounded readiness loop
  to finish before declaring failure.
- If a declarative update fails, inspect `system-update.service` and the relevant
  `nixos-rebuild` output. Do not replace the Nix-managed unit with a hand-written one.
