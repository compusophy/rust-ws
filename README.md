# Example TODO app with Rust and htmx

A simple example TODO app build using:

- [htmx](https://htmx.org/) for dynamic HTML updates
- [Rust](https://www.rust-lang.org/) for backend development
- [WebSockets](https://developer.mozilla.org/en-US/docs/Web/API/WebSockets_API) for real-time collaborative editing
- [SQLx](https://github.com/launchbadge/sqlx) and [SQLite](https://sqlite.org/) for data persistence
- [Rocket](https://rocket.rs/) web framework using handlebars templates
- [Bootstrap](https://getbootstrap.com/) for responsive UI components


## Run

```shell
cargo run
```

## Hot Reloading

Install cargo watch with `cargo install cargo-watch` then use:

```shell
cargo watch -x run
```

## Syntax Check

```shell
cargo clippy
```
