# apin - OpenAPI Navigator CLI

[![CI](https://github.com/ekvii/apin/actions/workflows/ci.yml/badge.svg)](https://github.com/ekvii/apin/actions/workflows/ci.yml)
[![Release](https://github.com/ekvii/apin/actions/workflows/release.yml/badge.svg)](https://github.com/ekvii/apin/actions/workflows/release.yml)

A terminal UI for navigating OpenAPI specs — fast, keyboard-driven, zero configuration.

Browse any number of specs with a Miller-columns path tree, inspect endpoints in a
full-screen detail view, and filter with incremental search — all without leaving
the terminal.

Supports **Swagger 2.0**, OpenAPI **3.0**, **3.1**, and **3.2**, YAML or JSON.

![apin demo](assets/apin.gif)

## Installation

### Homebrew (macOS and Linux)

```sh
brew install ekvii/apin/apin
```

### cargo install

```sh
cargo install apin
```

### Binaries (Linux, macOS, Windows)

[Latest release binaries](https://github.com/ekvii/apin/releases/latest) are available for Linux, macOS, and Windows. Download the binary for your platform and add it to your `PATH`.

### Build from source

Requires [Rust](https://rustup.rs) (stable).

```sh
cd apin
cargo build --release
./target/release/apin path/to/spec.yaml
```

## Usage

```sh
# single spec file
apin openapi.yaml

# entire directory — specs stream into the UI as they load
apin path/to/specs/

# HTTP(S) URL — probes well-known spec paths automatically
apin https://api.example.com

# explicit spec URL
apin https://api.example.com/openapi.yaml

# dir for downloaded specs (re-uses existing files on next run)
apin --download-dir . https://api.example.com

# force re-download even if the file already exists
apin --download-dir . --force-download https://api.example.com
```

## Features

- **Miller-columns tree** — navigate URL path segments column by column (`h` / `l`)
- **Endpoint detail** — method, summary, parameters, request body, response codes
- **Schema trees** — collapsible, interactive view of request body and response schemas; press `[N]` to open the Nth response tree
- **Deprecated markers** — deprecated operations and parameters are flagged with `[deprecated]` throughout
- **Webhooks** — 3.1/3.2 webhook entries appear alongside regular paths, labelled `[WEBHOOKS]`
- **Custom HTTP methods** — 3.2 `additionalOperations` (COPY, MOVE, QUERY, …) are parsed and displayed
- **Incremental search** — `/` to filter in any panel; `n` / `N` to cycle matches
- **Vim-style navigation** — `j`/`k`, `gg`/`G`, `Ctrl-D`/`Ctrl-U`
- **Multi-spec** — load a whole directory; switch between specs in a sidebar
- **URL input** — pass any HTTP(S) URL; apin probes well-known spec paths automatically
- **Download remote spec files** — `--download-dir` persists downloaded specs and skips re-downloading; use `--force-download` to override
