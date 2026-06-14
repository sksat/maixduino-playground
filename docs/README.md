# docs

Maixduino（K210）でベアメタル Rust を動かすまでに得た知見と罠のメモ。
一枚のファイルが肥大化しないよう、トピックごとに分割している。

- [toolchain.md](toolchain.md) — ビルド/書き込みツールチェーンの構成と、その理由
  （Rust target、`cargo-binutils`、`uv` で固定した `kflash`、`cargo run` での書き込み）。
- [hardware-maixduino.md](hardware-maixduino.md) — ボード固有事項: LED ピン、
  USB シリアルの配線、`uucp` グループ、`kflash -B maixduino`、カメラ/WiFi。
- [traps-linker-and-abi.md](traps-linker-and-abi.md) — rust-lld との格闘:
  riscv-rt の `link.x`、float ABI 不一致、`.eh_frame` 再配置オーバーフロー。
- [traps-crates-and-firmware.md](traps-crates-and-firmware.md) — `k210-hal` の
  crates.io 版 vs git 版、未完成の GPIO HAL、「MaixPy v4 は K210 用ではない」。

各罠には**エラーメッセージ原文**を残してある（後で grep できるように）。
