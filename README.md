# maixduino-test

**Sipeed Maixduino**（Kendryte K210）でベアメタル **Rust**。最初の「動いた」を2つ同時にやる:

ペリフェラルを1個ずつ触っていく実験リポジトリ。`src/main.rs` がその時の題材
（過去のは git 履歴に。シリアル出力は全デモ共通で UARTHS 115200）。

- **AES-128 ECB HW アクセラレータ**（現 `src/main.rs`）: k210-hal は `todo!()` スタブなので
  PAC 直叩きで実装。FIPS-197 のテストベクタを暗号化 → 既知の `69c4e0d8...c55a` と照合し
  シリアルに `PASS`。K210 の癖: `endian` レジスタを**鍵書き込みより先に**立てる（順序依存）、
  鍵は語順逆の LE、出力は LE。
- **SHA256 HW アクセラレータ**（コミット `301661d`）: PAC 直叩き。`SHA256("abc")` をハードで
  計算 → 既知値 `ba7816bf...` と照合。K210 の癖: 結果は語順逆＋バイトスワップ、`en` が done で落ちない。
- **CLINT `mtime` タイマ**（コミット `69cfde6`）: nop ループをやめて `mtime` で正確な 1Hz。
  周波数を UART ボーレートで自己校正 → `mtime_hz=7799258` ≈ CPU/50 ＝ ブート時 CPU ~390MHz。

> オンボード RGB LED(IO13) を光らせようとして大迷走した記録は
> [docs/finding-the-led.md](docs/finding-the-led.md)（結論: GPIO 制御は動くが、IO13 の
> RGB は 4.7K で極暗 or 個体死で視認できず。見える LED は IO6）。

ねらいは、CLI 中心の低レイヤ K210 ツールチェーンをきれいに組むことと、その過程で踏んだ
[大量のツールチェーン罠](docs/)を書き残すこと。

## クイックスタート

```sh
# 一度だけ: Arch でシリアルポートにアクセスする権限（実行後 再ログイン）
sudo gpasswd -a "$USER" uucp

# ビルド（ボード不要）
cargo build

# 接続済みボードへ書き込み（objcopy + kflash, flash.sh 参照）
cargo run

# 出力を見る
picocom -b 115200 /dev/ttyUSB0          # 終了は Ctrl-A Ctrl-X
```

シリアルに出るはずの内容:

```
hello
on
off
on
...
```

同時に IO13 の RGB 赤 LED も駆動しているが、**視認できておらず未確認**
（[docs/finding-the-led.md](docs/finding-the-led.md)）。

## 構成

| パス | 内容 |
|------|------|
| [src/main.rs](src/main.rs) | 本体: UARTHS の hello + GPIOHS の LED 点滅 |
| [memory.x](memory.x) | K210 SRAM 配置 **+ `.eh_frame` の破棄**（これが効いている！） |
| [.cargo/config.toml](.cargo/config.toml) | target・リンカ引数・`cargo run` のフラッシャ |
| [flash.sh](flash.sh) | `cargo run` のフック: ELF → `.bin` → `uv run kflash -B maixduino` |
| [rust-toolchain.toml](rust-toolchain.toml) | toolchain と target の固定 |
| [pyproject.toml](pyproject.toml) / [uv.lock](uv.lock) | `kflash` を `uv` で repo 内固定 |
| [docs/](docs/) | ツールチェーンのメモと罠ログ |

## ツールチェーン要約

- target は `riscv64imac-unknown-none-elf`（soft-float。K210 は FPU を*持っている*が、
  クレート群が soft-float なので。詳細は docs）
- ピンmux と UART は **git 版の `k210-hal`**（crates.io の `0.2.0` は古いスタブ）。
  LED は GPIO HAL が未完成なので **PAC レジスタ直叩き**で駆動
- `riscv-rt = "0.11"`（0.8 の `link.x` は今の rust-lld を壊す）
- 書き込みは `kflash`。`uv` で固定し `uv run kflash` で実行

おもしろいところ — float ABI の不一致、`(*(.trap))` のリンカスクリプト破綻、
K210 の SRAM が `0x8000_0000` にあるせいだけで踏む `.eh_frame` の再配置オーバーフロー —
は **[docs/](docs/)** に詳述。

## ステータス

**実機で確認済み**: ビルド / 書き込み（`uv run kflash -B maixduino`）/ シリアル出力（UARTHS）/
**IO6 の赤 LED 点滅**（Rust でも公式 MaixPy でも光る = GPIO 制御は両スタックで動作）。

**未決着**: ドキュメント上の RGB 赤 LED（IO13, 4.7K）の点灯。回路図上は正しく駆動しているが
視認できない —— IO6(510R, 明るい) の約10倍暗い ~0.3mA が見えないのか、個体の LED 死/断線か。
テスタ実測待ち。経緯は [docs/finding-the-led.md](docs/finding-the-led.md)。

カメラ(DVP)と WiFi(オンボード ESP32)は到達可能。HAL クレートに DVP 抽象は無いが、
`k210-pac` に DVP レジスタブロック（SCCB 含む）が有り、Rust の完動例
（[laanwj/k210-sdk-stuff](https://github.com/laanwj/k210-sdk-stuff) の `dvp-ov`, OV2640）も
存在する → 移植で行ける。詳細は [docs/hardware-maixduino.md](docs/hardware-maixduino.md)。

参照した回路図・データシートは [hardware/](hardware/) に確保（出典 dl.sipeed.com）。

> コード中のコメントは英語のままにしてある（必要なら日本語化する）。
