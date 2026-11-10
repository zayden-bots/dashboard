# Zayden dashboard

The operator dashboard for the Zayden Discord bot: a Topcoat server-rendered
web app on Postgres via `sqlx`. It builds on the `modules`, `platform` and
`schema` repos of the `zayden-bots` org and does not depend on `bots`.

## Layout

| Path                      | What it is                                                      |
| ------------------------- | --------------------------------------------------------------- |
| `web/`                    | Topcoat server-rendered operator dashboard                      |
| `docker/Dockerfile.web`   | release image of `web`                                          |
| `.sqlx/`                  | offline query cache; committed, regenerated on any query change |
| `bacon.toml`              | watch jobs, flags identical to CI                               |
| `deny.toml`               | `cargo-deny` licences, advisories and git sources               |
| `.github/workflows/ci.yml`| CI and the `images` job                                         |
| `docs/topcoat/`           | gitignored symlink to `~/Documents/Reference/topcoat/`          |
| `config.toml`             | gitignored symlink to `infra/config/zayden.toml`                |

The internal crates (`schema`, `zayden-app`, `zayden-core` and the feature
crates such as `accounts`, `youtube`, `patreon`) are git dependencies in the
root `Cargo.toml`. They track the default branch; `Cargo.lock` pins the commit.
Give every entry for the same org repository the same ref: two refs make cargo
treat the crates as different and their types stop matching. CI checks this
(step "One ref per org repository" in the `deny` job).

## Commands

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace --all-targets
cargo test --workspace
```

`--all-targets` defaults to _off_ and silently skips tests, benches and
examples. Always pass it.

Clippy never codegens or links, so it reports green while the build is broken.
Only a real build exercises linking.

`bacon.toml` defines these as watch jobs:

```bash
bacon                    # clippy, the default job
bacon test               # cargo test
bacon fmt                # cargo fmt --all --check
bacon build              # link check
bacon check-web          # web, check
bacon check-offline-web  # web, check with SQLX_OFFLINE=true
bacon clippy-web         # web, clippy
bacon test-web           # web, tests
bacon bundle-web         # web, topcoat asset bundle
bacon nextest             # needs `cargo install cargo-nextest`
```

Keep the flags identical across this README, `bacon.toml` and CI. A local
command weaker than CI is how drift starts.

## Web dashboard (`web/`)

`web` is a [Topcoat](https://github.com/tokio-rs/topcoat) 0.11 app: every page
renders on the server. Routes follow the module tree under `web/src/app/`
(Topcoat module routing: the module path is the URL), and `web::router()`
returns the builder that the binary and the `tests/` harness share.
It reads the `[dashboard]` keys of `config.toml` in the working directory and
the `DASHBOARD_*` environment variables.

It needs the Topcoat CLI, pinned to the same version as the crate:

```bash
cargo install topcoat-cli@0.11.0 --locked
```

Run it from the repo root against a throwaway database:

```bash
topcoat asset bundle -p web
DATABASE_URL=postgres://postgres:postgres@localhost:55432/zayden_prepare \
DASHBOARD_BIND_ADDR=127.0.0.1:3100 \
cargo run -p web
```

- **`topcoat asset bundle -p web`** builds the binary and writes its asset
  bundle (the compiled stylesheet and every other `asset!`) to `assets/` beside
  it, e.g. `target/debug/assets/`. The server refuses to start without a bundle,
  and a bundle only matches the build it came from: re-run the command after any
  change, before `cargo run`. `--release` bundles the release build. The CLI
  passes `RUSTUP_TOOLCHAIN`, `RUSTFLAGS` and `CARGO_TARGET_DIR` through to its
  inner build.
- **`DASHBOARD_BIND_ADDR`** overrides `[dashboard].bind_addr`. Discord OAuth
  redirects are registered for `:3000`, so log-in flows only complete when `web`
  runs there.
- **`DASHBOARD_COOKIE_KEY`** signs the one-shot result cookie shown after a
  save: standard base64 of at least 64 random bytes
  (`openssl rand -base64 64 | tr -d '\n'`). Set the same value on every
  instance. Unset, `web` logs a warning and signs with a key created at
  start, so a restart drops an unread result; a malformed value stops startup.
- **`TAILWIND_CLI`** points `web/build.rs` at a Tailwind CLI binary (v4.3.2).
  Unset, the build downloads that release itself (about 112 MB) and, on
  x86-64 Linux, checks it against a pinned SHA-256; on nightly the download
  repeats whenever the build script's output directory changes. Point it at a
  local copy to build offline; CI and `docker/Dockerfile.web` always set it to a
  checksummed download.
- Styles are utilities in the markup over the app tokens mapped in
  `web/styles.css`; `web/style/partials/` keeps the rules utilities cannot
  express (`docs/topcoat/ui.md`, "App styling"). Tailwind only sees
  class names written literally in `web/src/**/*.rs`.
- `view!` bodies are formatted by `topcoat fmt`; `topcoat fmt --check` exits
  non-zero when a file would change. Pass explicit paths
  (`topcoat fmt web/src web/tests`) so it skips `target/`, then run `cargo fmt`.

### Dev loop

```bash
export TAILWIND_CLI=/path/to/tailwindcss-4.3.2     # optional, see above
topcoat asset bundle -p web                        # after every change
cargo run -p web
topcoat fmt web/src web/tests && cargo fmt         # format view! bodies, then the rest
```

`bacon bundle-web` runs the bundle step, `bacon check-web` and `bacon clippy-web`
watch the crate. `cargo run` alone serves the bundle from the last
`topcoat asset bundle`, so a stale bundle shows stale CSS or a missing-asset
error.

### Tests

```bash
cargo test -p web
```

Most files in `web/tests/` render pages through the real router. The ones that
touch Postgres use `#[sqlx::test]`, which needs `DATABASE_URL` pointing at a
**throwaway** server (see Database below); `cargo test` creates and drops one
database per test. Nothing in `web` is feature-gated, so the plain workspace
commands run every test. `tests/legacy_*.rs` hold the source, markup and
stylesheet scans.

### Docker

`docker/Dockerfile.web` builds the release image on stable: a separate build target
downloads the Tailwind CLI and checks its SHA-256, the builder installs
`topcoat-cli`, cooks the dependencies with cargo-chef, removes
`rust-toolchain.toml` and runs `topcoat asset bundle --release -p web`, and the
runtime image copies `/app/web` and `/app/assets` side by side (the binary
looks for `assets/` next to itself). It runs as the `zayden` user, listens on
`0.0.0.0:3000` (`DASHBOARD_BIND_ADDR`) and reads `/app/config.toml`. It builds
with `SQLX_OFFLINE=true` and fetches the org git dependencies from GitHub.

```bash
docker build -f docker/Dockerfile.web -t dashboard .
```

CI publishes it as `dashboard`. The `infra` repo's `docker-compose.yml` runs it
as the `web` service on host port `3100` with `config/zayden.toml` mounted at
`/app/config.toml`.

## Conventions

- Lints are declared once in the root `Cargo.toml` under `[workspace.lints]`;
  every crate opts in with `[lints] workspace = true`.
- Dependency versions are pinned once in `[workspace.dependencies]`; member
  crates say `foo.workspace = true` and may _add_ features on top, but never
  restate a version. Unused workspace entries cost nothing — cargo only
  resolves a dependency once a member opts in.
- `unwrap`/`expect`/`panic`/`todo`/`dbg!` are **denied** outside tests;
  `clippy.toml` re-permits them under `#[cfg(test)]`.
- Tests live in `tests/`, not in inline `#[cfg(test)] mod tests` blocks.
- `rustfmt.toml` uses unstable options, so `rust-toolchain.toml` pins nightly
  for local work. Never write `cargo +nightly` — the prefix is redundant.
- The shared tooling files (`rust-toolchain.toml`, `rustfmt.toml`,
  `clippy.toml`, `.cargo/config.toml`) are owned by `infra/template/`; copy
  changes there, not here. `../infra/scripts/sync-standards.sh --check` reports
  drift.

## Toolchain

Nightly for local development, stable for release images. `Cargo.toml` must
stay stable-parseable, and `.dockerignore` must keep excluding `.cargo/` and
`rust-toolchain.toml`.

## CI

`.github/workflows/ci.yml` runs these jobs in parallel: `rustfmt` (nightly),
workspace `clippy`, workspace tests against a Postgres service, `cargo-deny`
(plus the one-ref-per-org-repository check), and `sqlx prepare --check`. `web`
is covered by the workspace `clippy` and `test` jobs. Jobs that compile `web`
set `TAILWIND_CLI` to a checksummed Tailwind download. Everything except
`rustfmt` builds on **stable** (`RUSTUP_TOOLCHAIN=stable` outranks
`rust-toolchain.toml`, and `RUSTFLAGS=""` clears the nightly-only flags in
`.cargo/config.toml`).

The `images` job in the same workflow builds the `web` release image once
`rustfmt`, `clippy` and `cargo-deny` pass, and pushes it to GHCR as
`dashboard` for every event except pull requests.

## Database

Stand up a throwaway server, then apply the migrations the lockfile pins (what
CI does):

```bash
docker run --rm -d --name zayden-prepare -p 55432:5432 -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=zayden_prepare postgres:18-alpine
export DATABASE_URL="postgres://postgres:postgres@localhost:55432/zayden_prepare"
dir="$(cargo metadata --format-version 1 --locked \
  | jq -r '.packages[] | select(.name == "schema") | .manifest_path | rtrimstr("/Cargo.toml")')"
sqlx migrate run --source "$dir/migrations"
```

With the `schema` checkout next to this repo, `sqlx migrate run --source
../schema/migrations` works too.

**Only ever run mutating `sqlx` commands against a throwaway database.** The
`DATABASE_URL` in `.env` points at the live one; migrations there are the user's
call.

`.sqlx/` must be committed and holds only this repo's own queries: CI sets
`SQLX_OFFLINE=true`, so that cache is the only thing the query macros have to
check against. A git dependency's queries are checked against the `.sqlx/`
inside that dependency's checkout. Regenerate it against an **empty,
freshly-migrated** database — `LEFT JOIN` nullability inference is
plan-sensitive, and a cache built against a populated DB will fail CI's
`prepare --check`. Two steps; the first builds the git dependencies offline so
their queries do not leak into this cache:

```bash
SQLX_OFFLINE=true cargo check --workspace --all-targets
cargo sqlx prepare --workspace -- --all-targets
```

If prepare fails, run `git restore .sqlx`.

## Taking an upstream change

Push upstream first. Then, in this repo, run
`cargo update schema zayden-app accounts` (one crate per org repo moves that
whole repo; move all three together so the module queries and the schema they
were checked against stay in step), regenerate `.sqlx/` if queries changed, run
the checks and commit `Cargo.lock`.

For local cross-repo work without pushing, `../infra/scripts/dev-link.sh on`
writes `../.cargo/config.toml` with `[patch]` entries pointing every org crate
at the sibling checkouts; `off` removes it; `status` reports. Never commit a
`Cargo.lock` written while the link is on: run `dev-link.sh off` and
`cargo update <crate>` (or `git restore Cargo.lock`) first.
