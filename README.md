# apin - OpenAPI Navigator CLI

[![Release](https://github.com/ekvii/apin/actions/workflows/release.yml/badge.svg?branch=main)](https://github.com/ekvii/apin/actions/workflows/release.yml)

A terminal UI for navigating OpenAPI specs — fast, keyboard-driven, zero configuration.

Browse any number of specs with a Miller-columns path tree, inspect endpoints in a
full-screen detail view, and filter with incremental search — all without leaving
the terminal.

## Usage

```sh
# single spec file
apin openapi.yaml

# entire directory — specs stream into the UI as they load
apin path/to/specs/
```

Supports OpenAPI **3.0** and **3.1**, YAML or JSON.

## Features

- **Miller-columns tree** — navigate URL path segments column by column (`h` / `l`)
- **Endpoint detail** — method, summary, parameters, request body, response codes
- **Schema tree** — collapsible, interactive view of request body schemas
- **Incremental search** — `/` to filter in any panel; `n` / `N` to cycle matches
- **Vim-style navigation** — `j`/`k`, `gg`/`G`, `Ctrl-D`/`Ctrl-U`
- **Multi-spec** — load a whole directory; switch between specs in a sidebar

## Build from source

Requires [Rust](https://rustup.rs) (stable).

```sh
cd apin
cargo build --release
./target/release/apin path/to/spec.yaml
```
