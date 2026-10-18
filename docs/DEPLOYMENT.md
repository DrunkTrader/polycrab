# Deployment

PolyCrab is a single local/host process. There is no hosted trading service,
container registry workflow, Docker Compose stack, database, or local sidecar.

## Docker

The multi-stage [`Dockerfile`](../Dockerfile) builds the pinned Rust binary and
creates a small Debian runtime image. Build it with:

```bash
docker build -t polycrab .
```

Run the default simulated worker:

```bash
docker run --rm -it polycrab run copy-trading
```

Run the TUI while mounting credentials read-only:

```bash
docker run --rm -it \
  -v "$PWD/config.yaml:/app/config.yaml:ro" \
  polycrab tui
```

The image contains `config.json` and `config.yaml.example`, but not a private
`config.yaml`. Never bake credentials into an image. For live mode, provide
secrets through a protected mount or environment mechanism and explicitly set
both live-trading flags in the JSON configuration.

## Release archives

`.github/workflows/release.yml` builds archives when a `v*` tag is pushed, or
when manually dispatched for an existing tag. It uses Rust `1.99.0`, builds the
configured Linux, macOS, and Windows targets, and creates draft GitHub Releases
with SHA256 checksums.

Release archive names follow:

```text
polycrab-<tag>-<target>.tar.gz
polycrab-<tag>-<target>.zip
```

Each archive includes the `polycrab` executable, root README, this `docs/`
directory, `LICENSE`, `config.json`, and `config.yaml.example`.

To trigger a release after reviewing the tag:

```bash
git tag <tag>
git push origin <tag>
```

Do not push or create a release as part of ordinary local development.
