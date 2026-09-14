# Private PDF renderer deployment

This optional Compose example runs the PDF renderer on a Docker-internal network. It pins
Gotenberg 8.37.0 Chromium by multi-platform digest and does not publish a port in the production
configuration. PDF export remains disabled in Agent Note until an operator enables the matching
`[export.pdf]` configuration.

This example is a deployment baseline, not evidence that the release PDF qualification suite has
passed. Run that suite separately against this exact image before enabling PDF export for users.

## Security and resource boundary

The renderer has no host mounts and runs with a read-only root filesystem, all Linux capabilities
dropped, `no-new-privileges`, PID/CPU/memory/shared-memory limits, and an ephemeral `/tmp`. The
`agent-note-pdf` network is `internal`, so Docker does not give the renderer external egress.
Attach only the Agent Note application container to this network; do not attach databases,
metadata services, or other workloads.

Gotenberg is also configured to:

- disable JavaScript, `downloadFrom`, webhooks, and non-Chromium PDF-engine routes;
- deny both public and private Chromium destinations;
- ignore environment proxies and carry no proxy or browser credentials;
- clear Chromium cache, cookies, and local storage between conversions;
- admit at most two Chromium conversions, including active and queued work; and
- cap requests at 30 seconds and multipart bodies at 48 MB.

The 48 MB renderer body ceiling accommodates Agent Note's bounded 8 MiB generated HTML, 32 MiB
combined assets, footer, and multipart metadata. Agent Note independently limits two in-flight
exports, its end-to-end deadline to 35 seconds, renderer time to 30 seconds, and PDF output to
32 MiB. Keep `config.toml.example` and the Compose limits aligned if those application limits are
reduced.

Defense is layered: the Gotenberg URL filters block author-controlled HTTP(S) fetches, while the
internal Docker network blocks deployment-level egress. Neither control replaces the other.
Packaged request files remain under Gotenberg's temporary request directory, and the application
must continue using only `/forms/chromium/convert/html` without caller-supplied conversion,
download, webhook, proxy, cookie, or credential options.

## Fonts

The pinned image already contains the fonts used by the export document; no network install or
host font mount is needed. Verify the selected image on every digest update:

```sh
image='gotenberg/gotenberg:8.37.0-chromium@sha256:0d28ae9a96441588ef739623726bd500ad0720b77266c6f1351a13e333fbd61c'
docker run --rm --entrypoint sh "$image" -lc \
  "fc-match 'Noto Sans CJK SC'; fc-match 'Noto Sans CJK JP'; fc-match 'DejaVu Sans'"
```

Expected families are `Noto Sans CJK SC` for Simplified Chinese, `Noto Sans CJK JP` for Japanese,
and `DejaVu Sans` for the Latin fallback. Font presence is not a substitute for inspecting glyphs
in every rendered fixture page.

## Start and connect Agent Note

Start the renderer first:

```sh
docker compose -f deployment/pdf-export/compose.yaml pull
docker compose -f deployment/pdf-export/compose.yaml up -d --wait
```

If Agent Note is managed by another Compose project, attach its service to the already-created
network by extending that project's configuration:

```yaml
services:
  agent-note:
    networks:
      - default
      - pdf-export

networks:
  pdf-export:
    external: true
    name: agent-note-pdf
```

Copy the values from `config.toml.example` into the application configuration selected by
`NOTE_CONFIG_PATH`, then restart only Agent Note. From the application container, verify that
`http://gotenberg:3000/health` is reachable. Do not expose port 3000 or attach Gotenberg to the
application's ingress network.

Check the effective container controls after startup:

```sh
docker compose -f deployment/pdf-export/compose.yaml config
docker inspect --format \
  'readonly={{.HostConfig.ReadonlyRootfs}} caps={{json .HostConfig.CapDrop}} security={{json .HostConfig.SecurityOpt}} pids={{.HostConfig.PidsLimit}} memory={{.HostConfig.Memory}} network={{.HostConfig.NetworkMode}}' \
  pdf-export-gotenberg-1
```

The generated container name varies if `COMPOSE_PROJECT_NAME` is set; use
`docker compose ps -q gotenberg` instead when scripting inspection.

## Local validation

For renderer behavior and PDF quality checks driven by a host process, the validation override
publishes only on loopback at port 33000:

```sh
docker compose \
  -f deployment/pdf-export/compose.yaml \
  -f deployment/pdf-export/compose.validation.yaml \
  up -d --wait
curl --fail --silent --show-error http://127.0.0.1:33000/health
```

Use `GOTENBERG_VALIDATION_PORT` to select another loopback port. Never change the mapping to
`0.0.0.0` or an externally reachable host address.

Docker port forwarding does not work from an internal-only bridge, so this override necessarily
changes the validation network to a normal bridge. It therefore **must not** be used to claim that
deployment-level egress is blocked. Stop it after the host-driven checks, restart the base Compose
file, and run renderer-isolation qualification from a client container attached only to
`agent-note-pdf`:

```sh
image='gotenberg/gotenberg:8.37.0-chromium@sha256:0d28ae9a96441588ef739623726bd500ad0720b77266c6f1351a13e333fbd61c'
docker run --rm --network agent-note-pdf \
  --entrypoint curl "$image" \
  --fail --silent --show-error http://gotenberg:3000/health
```

Run the PDF fixture client on that network as well and use `http://gotenberg:3000` as its renderer
URL. Confirm the effective network has `internal=true` and that outbound requests fail. Only this
base topology supplies deployment-level egress denial.

## Operations and rollback

Monitor Gotenberg logs, health, CPU/memory use, conversion latency, rejection rate, and Agent Note's
typed renderer errors. A renderer health response is readiness evidence, not proof that PDF export
or network isolation works; the real-renderer suite must exercise both.

To roll back, first set `[export.pdf].enabled = false` and restart Agent Note. Markdown export and
normal note use remain available. After in-flight requests have drained, stop the optional
renderer:

```sh
docker compose -f deployment/pdf-export/compose.yaml down
```

This removes the renderer container and Compose-owned network. It does not remove images, volumes,
notes, attachments, or databases. There is no schema or data rollback.

## Gotenberg references

- [Chromium HTML conversion](https://gotenberg.dev/docs/convert-with-chromium/convert-html-to-pdf)
- [Configuration](https://gotenberg.dev/docs/configuration)
- [Outbound URL filtering](https://gotenberg.dev/docs/outbound-url-filtering)
