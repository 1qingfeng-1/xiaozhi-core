# 解析器模糊测试

针对 WS / UDP / MQTT / OTA / 服务器消息解析器的覆盖引导 fuzz。
它与 `tests/properties.rs`（proptest 随机输入）互补：proptest 验证性质，
fuzz 用覆盖率引导探索边界。

## 前置

```bash
cargo install cargo-fuzz --locked
rustup toolchain install nightly
```

## 运行

**必须从仓库外启动**：父仓库 `.cargo/config.toml` 的 `build-std` 会被
Cargo 向上查找继承，导致 `-Zbuild-std` 与 sysroot 的 `core` 冲突
（`duplicate lang item`）。用 `--fuzz-dir` 指向本目录即可：

```bash
cd /tmp
FUZZ=/path/to/xiaozhi-core/fuzz
cargo +nightly fuzz run parse_ws   --fuzz-dir "$FUZZ" -- -runs=10000
cargo +nightly fuzz run parse_udp  --fuzz-dir "$FUZZ" -- -runs=10000
cargo +nightly fuzz run parse_mqtt --fuzz-dir "$FUZZ" -- -runs=10000
cargo +nightly fuzz run parse_text --fuzz-dir "$FUZZ" -- -runs=10000
```

已验证：四个 target 各 1000 次运行无崩溃（nightly 1.100 / cargo-fuzz 0.13.2）。

发现崩溃后，最小化输入并作为回归用例加入 `tests/properties.rs` 或单元测试。

## CI

模糊测试不进入 GitHub Actions（耗时且需要 nightly sanitizer）。
PR 前建议至少跑一次 10k 次迭代；corpus/artifacts 已在 `.gitignore` 中。

## 目标

| target | 覆盖 |
|---|---|
| `parse_ws` | `ws::header_len` / `ws::parse_header` |
| `parse_udp` | `udp::parse_header` + 序号跟踪 |
| `parse_mqtt` | `mqtt::decode` |
| `parse_text` | server_msg / OTA manifest / HTTP / JSON 扫描 |
