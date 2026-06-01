# Implement Yar Tracing on Native PHP 7.3.15 + yar 2.1.2

## Summary
Use a source-built native runtime on this Debian 13 host to guarantee `PHP 7.3.15`, `php-fpm`, `phpize/php-config`, and `yar 2.1.2`, then implement full Yar tracing for the HTTP transport with a real end-to-end test against an independent Yar server process.

The deliverable includes:
- native toolchain and runtime installation
- `Yar_Client` client tracing with `sw8` injection
- `Yar_Server` server-side extraction and entry-span creation
- a real test topology where FPM code calls a separate Yar service over HTTP

## Environment Setup
### System packages
Install the build and runtime prerequisites on Debian 13:
- base tools: `build-essential`, `autoconf`, `automake`, `libtool`, `re2c`, `bison`, `pkg-config`, `curl`, `git`, `unzip`
- PHP build deps commonly needed by this repo and Composer packages:
  - `libxml2-dev`, `libsqlite3-dev`, `zlib1g-dev`, `libssl-dev`, `libcurl4-openssl-dev`
  - `libonig-dev`, `libzip-dev`, `libreadline-dev`, `libxslt1-dev`
  - `libjpeg-dev`, `libpng-dev`, `libwebp-dev`, `libfreetype-dev`
- repo Rust deps already used by CI:
  - `llvm-18-dev`, `libclang-18-dev`, `protobuf-compiler`, `libsasl2-dev`
- test/runtime deps:
  - `docker-compose-plugin` or working `docker compose`
  - `composer` or a pinned Composer PHAR

### PHP 7.3.15 source build
Build and install PHP 7.3.15 from source under an isolated prefix, for example `/opt/php-7.3.15`, with:
- CLI
- FPM
- phpize/php-config
- extensions needed by this repo's current test harness where feasible:
  - `mbstring`, `json`, `xml`, `dom`, `pdo`, `pdo_mysql`, `mysqli`, `opcache`, `zip`, `phar`, `ctype`, `tokenizer`

Set execution env for the repo test harness:
- `PHP_BIN=/opt/php-7.3.15/bin/php`
- `PHP_FPM_BIN=/opt/php-7.3.15/sbin/php-fpm`
- prepend `/opt/php-7.3.15/bin` to `PATH`

### yar 2.1.2 installation
Install `yar-2.1.2` against that exact PHP build using the matching `phpize/php-config`.
Enable `yar.so` for both CLI and FPM via the same `php.ini` or `-d extension=yar.so`.

Verification target:
- `php --version` reports `7.3.15`
- `php-fpm --version` reports `7.3.15`
- `php -m` and `php-fpm -m` include `yar`
- `php --ri yar` reports `2.1.2`

## Implementation Changes
### Client plugin
Add a new `plugin_yar` module and register it in `src/plugin/mod.rs`.

Hook coverage:
- `Yar_Client::__construct`
- `Yar_Client::__call`
- `Yar_Client::call`

Behavior:
- create one exit span per outbound RPC
- operation name: `Yar_Client->{method}`
- peer: parse from target URI host:port, fallback `unknown:0`
- span layer: `RpcFramework`
- component id: continue using `COMPONENT_PHP_ID`
- tags:
  - `rpc.system=yar`
  - `rpc.service`
  - `rpc.method`
  - `url`
- merge `sw8` into Yar request options/headers without dropping user values
- mark error and log details on exception, transport failure, or remote fault

### Server-side Yar entry support
Hook coverage:
- `Yar_Server::__construct` if needed for metadata
- `Yar_Server::handle` as request boundary

Behavior on `handle` begin:
- read Yar request metadata/header carrier for HTTP transport
- extract `sw8`
- derive operation name as `{service}.{method}` or fallback `Yar_Server:{method}`
- create a dedicated request context for the Yar invocation
- set tags:
  - `rpc.system=yar`
  - `rpc.protocol=yar`
  - `rpc.service`
  - `rpc.method`

Behavior on `handle` end:
- finish the Yar request context
- mark error on exception or Yar fault response
- keep it isolated from the outer FPM HTTP request context

### Request/context support
Extend `src/request.rs` and possibly `src/context.rs` with an RPC-oriented lifecycle API:
- create RPC request context from `(request_id, propagation_header, operation_name)`
- finish RPC request context with an error flag instead of HTTP status
- keep existing FPM/Swoole HTTP init/shutdown logic unchanged

### Conflict handling with HTTP root spans
Because Yar server runs over HTTP/FPM, the process may already have an outer HTTP entry span.

Rule:
- keep the existing HTTP request context as-is
- create the Yar handling span as an inner request-bound tracing context for Yar execution
- ensure nested plugin spans inside Yar handler attach to the Yar trace context

## Test Plan
### Runtime topology
Add a real end-to-end Yar test with an independent server process:
- a standalone PHP server script running `Yar_Server` over HTTP on its own port
- a normal FPM test page that instantiates `Yar_Client` and calls that service
- collector validation that confirms trace propagation from FPM client span to Yar server span

### Repo test changes
Add:
- one new FPM PHP fixture for outbound client call
- one new standalone Yar server fixture script
- Rust test harness updates to spawn/stop the Yar server process during `tests/e2e.rs`
- expected collector assertions for:
  - outbound exit span
  - inbound Yar entry span
  - parent/child relationship via propagated `sw8`

### Scenarios to cover
- successful Yar HTTP RPC call
- remote exception/fault from server
- missing propagation header still creates a fresh entry span
- user-defined headers/options remain intact after `sw8` injection
- invalid or partial URI falls back safely without panic

## Assumptions
- Exact versions are fixed: `PHP 7.3.15` and `yar 2.1.2`.
- Transport scope is only Yar over HTTP in v1.
- Real verification must use a separate Yar service process.
- Existing non-Yar plugin behavior and HTTP/Swoole tracing remain unchanged.
- On Debian 13, PHP must be source-built rather than installed from default apt repositories.
