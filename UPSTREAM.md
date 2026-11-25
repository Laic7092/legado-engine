# Upstream

The rule engine and the JavaScript host layer are derived from
[`hadc188/reader`](https://github.com/hadc188/reader) (MIT), commit
`887bb6b903baa36e9ebaac2e2b8f4ff74d23e5b4`.

- Copied: `src/model`, `src/parser`, `src/crawler`, `src/util`, `src/error`
- Removed from `src/parser/mod.rs`: `http_tts_template`
- Replaced `src/service` with stubs for the browser-login surface
- Added: `src/pipeline.rs`, `src/abi.rs`, `legado.h`, `examples`, `tests`

Rule and host-API semantics follow the Kotlin reference implementation
([`LegadoTeam/legado`](https://github.com/LegadoTeam/legado)); `hadc188/reader`
is the Rust source of the copied modules.

The copied tree keeps upstream formatting. Do not run `cargo fmt` over it;
`make fmt` formats only the files added here.
