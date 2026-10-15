# fenders-cli

`fenders` is the command line for [Fenders](https://fenders.io): deploy and run WebAssembly
(WASI) components in hardware-isolated microVM sandboxes, built on the
[Fenders customer API](https://platform.fenders.io/api/openapi.json).

It ships as a single native binary with no runtime dependencies.

```
    ▐▌
  ▄████▄
  ██████    fenders.io
  ▀▀▀▀▀▀    WASM sandboxes in hardware-isolated microVMs
  ██████
  ▀▀▀▀▀▀
  ▀████▀
```

## Quick start

```sh
fenders auth login                       # paste a workspace API key (Settings → API keys)
fenders use                              # pick a workspace and project
fenders deploy app.wasm --http           # upload → inspect → deploy → wait → print URL
fenders logs app -f                      # follow its output
fenders run app --logs                   # trigger a manual run and print its logs
```

Deploying again to an existing app creates a new version and releases it:

```sh
fenders deploy app.wasm --app app -e GREETING=hello --size medium
```

## Commands

| Area | Commands |
|---|---|
| Session | `auth login \| logout \| status \| token`, `use [WORKSPACE[/PROJECT]]`, `context` |
| Ship | `deploy FILE.wasm`, `run APP`, `logs APP [-f]` |
| Apps | `apps list \| get \| create \| start \| stop \| url \| connections \| set-connections` |
| App history | `versions …`, `releases …`, `runs …`, `events APP`, `triggers get \| set` |
| Project | `projects list \| create`, `workspaces list`, `artifacts list \| get \| inspect \| upload`, `connections list \| get \| create https\|tcp \| revoke`, `secrets list \| get \| create`, `ops list \| get \| wait`, `usage`, `sizes`, `https-access show \| validate`, `schedule preview` |
| Anything else | `api PATH [-X METHOD] [-d JSON\|@file]` with `{workspace}`/`{project}` placeholders |
| Shell | `completion bash\|zsh\|fish\|powershell\|elvish` |

Run `fenders <command> --help` for flags and examples.

## How it behaves

- **Names or ids.** Workspaces, projects, apps, connections and secrets can be given by name;
  the CLI turns names into the API's 32-character ids.
- **Context.** Workspace and project come from `--workspace/--project`, then
  `FENDERS_WORKSPACE`/`FENDERS_PROJECT`, then `fenders use`, and finally an interactive
  picker if you're in a terminal.
- **Waiting.** Deploys, releases, runs and lifecycle changes are background operations.
  The CLI waits for them with a spinner by default. `--no-wait` prints the operation instead;
  pick it up later with `fenders ops wait ID`.
- **Safe retries.** Each invocation sends one `Idempotency-Key`, reused when it retries on
  network errors, 429 or 5xx responses, so a retried deploy never runs twice.
- **Revisions.** Mutations that need `expected_revision` read the current revision
  automatically. Pass `--expected-revision N` to pin one.
- **Output.** Tables and colour on a terminal; JSON when piped, or with `--json` / `-o json`.
  `fenders logs` writes one JSON record per line, with base64 output decoded into `text`.
  `NO_COLOR` and `--no-color` are honoured. Progress and errors go to stderr.
- **Secrets.** Values are never accepted as flags: they're read from stdin, `--from-file`,
  or a hidden prompt.
- **Errors.** API errors show the message, a hint about what to do, and the `request_id`.
- **Scripting.** `--no-input` disables prompts, and exit status is non-zero on any failure
  (including a failed operation).

## Configuration

| Setting | Flag | Environment | Stored in |
|---|---|---|---|
| API key | — | `FENDERS_API_KEY` | macOS Keychain / Windows Credential Manager; elsewhere `credentials.toml` (mode 0600) |
| API URL | `--api-url` | `FENDERS_API_URL` | `config.toml` (`api_url`) |
| Workspace / project | `-w` / `-p` | `FENDERS_WORKSPACE` / `FENDERS_PROJECT` | `config.toml` |
| Config directory | — | `FENDERS_CONFIG_DIR` | default `~/.config/fenders` (`%APPDATA%\fenders` on Windows) |

The default API URL is `https://platform.fenders.io/v1`, which is the spec's relative server
URL (`/v1`) resolved against where the spec is hosted. Override it with `--api-url` if your
deployment uses a different base.

API-key management (`/workspaces/{id}/api-keys`) requires a workspace-owner console session,
so it is not exposed as a command. Manage keys in the console.

## Building

```sh
cargo build --release          # → target/release/fenders (~4 MB, stripped, LTO)
cargo test                     # unit tests + HTTP behaviour against a mock server
```

Release binaries are built natively per platform by `.github/workflows/release.yml` when a
`v*` tag is pushed: macOS (arm64, x86_64), Linux (static musl, x86_64 and arm64) and Windows.
TLS uses rustls, so there is no OpenSSL dependency.

## Layout

```
src/
  main.rs        command tree (clap), global flags, error reporting
  client.rs      HTTP client: auth, idempotency, retries, error mapping, wasm upload
  app.rs         context resolution, name → id lookup, pagination, operation polling
  output.rs      tables / records / JSON rendering of untyped API records
  ui.rs          brand palette, banner, spinners, prompts theme
  auth.rs        API key storage (keychain or 0600 file)
  config.rs      config file
  cmd/           one module per command group
```
