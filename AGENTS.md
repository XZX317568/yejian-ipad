# AGENTS.md

## 推送前必须通过本地检查

任何推送/合并前，请在 `rust-reader/` 下确认通过（CI 会在每次 push 和 PR 时运行同样的检查）：

```sh
cargo test --locked --workspace -j 2 -- --test-threads=1
cargo check --release --locked -p yejian --bins
```

平台相关改动还需：

```sh
cargo check --release --locked -p yejian --target aarch64-apple-ios      # macOS
cargo check --release --locked -p yejian-platform --target aarch64-linux-android
```

工具链版本以 `.github/workflows/` 中 `RUSTUP_TOOLCHAIN`（当前 1.97.0）为准；`Cargo.toml` 的 `rust-version` 为 1.95。
