# ツールチェーン: 非自明な判断だけ

組み方自体は `Cargo.toml` / `.cargo/config.toml` / `flash.sh` / `pyproject.toml` を
読めば分かるので、ここには*なぜそうしたか*だけ書く。

- **target は soft-float `riscv64imac`**（ISA 的に正しい `gc` ではなく）。理由は
  [traps-linker-and-abi.md](traps-linker-and-abi.md) §2。一番ハマる選択なので最初に確認。

- **リンカは stock の `rust-lld`、GNU binutils 不要**。これが成立しているのは
  [`memory.x`](../memory.x) の `.eh_frame` 破棄のおかげ（[同 §3](traps-linker-and-abi.md)）。
  `memory.x` を編集するときはあの `SECTIONS{/DISCARD/}` を消さないこと。

- **`kflash` は `uv` で repo 内固定**（`uv tool install` でグローバルに入れない）。
  フラッシャのバージョンを repo と一緒に再現可能にするための意図的な選択。
  `uv run kflash` で呼ぶ。
