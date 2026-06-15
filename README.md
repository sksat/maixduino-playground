# maixduino-test

**Sipeed Maixduino**（Kendryte K210）でベアメタル **Rust**。最初の「動いた」を2つ同時にやる:

ペリフェラルを1個ずつ触っていく実験リポジトリ。`src/main.rs` がその時の題材
（過去のは git 履歴に。シリアル出力は UARTHS、基本 115200／現カメラデモは 1.5Mbaud）。

- **カメラ (OV2640/DVP) — 320×240 RGB565 撮影**（コミット `f107bf9` + [src/dvp.rs](src/dvp.rs)）:
  RGB565 フレームを DVP（自前 AXI マスタ）で SRAM に取り込み → シリアルにダンプ → ホスト `uv run python`
  で PNG 化（`captures/`、未コミット）。実機で**クリーンな実写真**（何が写ってるか分かる、確実）。
  ハマり所: ① **カメラ FFC の逆挿し**で全 SCCB が 0xff（未接続と同症状）、② **PCLK が速すぎて水平に化ける**
  → XCLK 分周を 3→7 に下げて解決。DVP には**キャッシュ有りアドレス**を渡し CPU は**無しエイリアス
  (0x4000_0000)**で読む。DVP/SCCB ドライバは [laanwj/k210-sdk-stuff](https://github.com/laanwj/k210-sdk-stuff)
  から移植。
- **ESP32（オンボード WiFi）リンク確立 — step 1**（現 `src/main.rs`）: K210 とオンボード ESP32-WROOM-32 の
  疎通確認。配線は UART（**IO6=ESP32_U0TX→K210 RX, IO7=K210 TX→ESP32_U0RX**）と SPI/nina（CS=IO9, SPI0
  SCLK=IO27/MOSI=IO28, READY=IO25）。**IO8=ESP32_EN** をパルスして ESP32 をリセット → UART1(115200) で
  **ブートバナーを捕捉**（`ets Jun 8 2016 / POWERON_RESET / SPI_FAST_FLASH_BOOT … entry 0x4008068c`）→
  ESP32 が生きててフラッシュにファームが居ることを確認。続けて `AT\r\n` を3回投げても**無応答** ＝ esp-at では
  なく **nina-fw（SPI 版 WiFiNINA）**。よって WiFi は UART AT ではなく **SPI nina プロトコル**で叩く（次ステップ）。
  ピン配線は [hardware/](hardware/) の回路図/配線表（xlsx）から確定。
- **RTC（リアルタイムクロック）+ mtime クロスチェック**（コミット `a82fbd0` + [src/rtc.rs](src/rtc.rs)）:
  HAL に `rtc` は無いので PAC 直叩き（kendryte SDK の手順を移植）。壁時計を 2026-06-16 12:00:00 にセットして
  1Hz で刻ませ、**独立した CLINT `mtime` と突き合わせて 1Hz を裏取り**。RTC は 26MHz クリスタルを
  `initial_count=26_000_000` で割って 1秒、`register_ctrl` の write/read_enable とマスクで書込/読出を切替。
  実機で **RTC 1秒 = mtime 約 7,800,000 ティック**（6サンプルのばらつき僅か7ティック、期待 ~7.80M=CPU/50 と一致）→
  クリスタル由来の RTC と PLL 由来の CPU クロックが 0.01% で一致して相互検証 → `PASS`。
- **カメラ — 動画ストリーム（解像度ランタイム切替）**（コミット `0cd01a9` + [tools/stream.py](tools/stream.py)）:
  OV2640 のフレームを連続ダンプしてシリアル動画に。**解像度は再フラッシュ不要でホストから切替** ──
  UARTHS の **RX**（io4）でコマンドバイトを受け、`'1'`=QQVGA 160×120 / `'2'`=QVGA 320×240 / `'3'`=VGA 640×480
  を `get_image` の合間にポーリングして OV2640+DVP を再構成（フレームバッファは VGA 分を確保し小解像度はその先頭を使用）。
  ヘッダ `IMGSTART <w> <h>` が現在サイズを運ぶのでホストは自動追従。**fps はシリアル帯域（1.5Mbaud≈120KB/s）で決まる**:
  QQVGA(38KB)≈**2.4fps** / QVGA(154KB)≈0.8fps / VGA(614KB)≈0.2fps。`tools/stream.py` が数秒録って実 fps を測り
  **ffmpeg で mp4 化**（DISPLAY 無し環境でも可。ライブ視聴は `ffplay` にパイプ）。本物の動画には WiFi 等の高速転送が要る。
- **カメラ — VGA 640×480 RGB565 撮影**（コミット `0bebeba`）: クリーン RGB の最大解像度（QVGA の4倍）。
  ハマり所: **出力サイズ(0x5a/0x5b)だけ上書きすると `frame_finish` がハングする** ── センサ読み出し窓
  （0x17/0x18/0x19/0x1a/0x32）・DSP スケーラ（0xc0/0xc1/0x50-0x5c）・PCLK 分周（0xd3）を**全部まとめて**
  動かす必要がある。ArduCAM の `640x480_JPEG` の窓/スケーラ表を流用しつつ、最終フォーマットだけ
  JPEG(0xda=0x10)→ RGB565(0xda=0x08) に差し替え（smart-friend/codex と協働で導出）。
  **シリアルは UARTHS 1.5Mbaud**（614400B が**約5秒/枚**、115200 比 約10倍）。以前「高ボーレートは化ける」と
  していたのはホスト側の早すぎる timeout が原因で、1.5M では IMGSTART ヘッダもフレームもバイト単位でクリーン。
  分周は `cpu/baud-1`（cpu=390MHz 固定）で 1.5M→div 259＝厳密に 390e6/260。2M/3M も厳密分周だが化ける
  （io5 の信号品質限界、1.5M が上限。kflash の書き込みも同じ UARTHS を 1.5M で使うので実績あり）。
  ホスト側 [tools/grab.py](tools/grab.py) は **numpy でベクトル化**（RGB565展開・複数枚 temporal median・
  per-row destripe・PNG 化）── 純Python では 640×480 で約60秒かかっていた処理が**0.1秒未満**。
  `uv run python tools/grab.py --frames 5 --out captures/clean.png`（5枚 median の実写真が合計 ~28 秒）。
- **カメラ — UXGA 1600×1200 JPEG 撮影（最大解像度）**（コミット `f5dbb55`）: OV2640 を JPEG/UXGA に設定
  （ArduCAM レジスタ表）、DVP で JPEG バイト列を取り込み、**デバイス上で SOI/EOI(FF D8…FF D9)を探して
  JPEG 部分だけダンプ**（圧縮済み ~150KB）。**正しい UXGA JPEG**（FFD8FFE0…FFD9、上部に実シーン）。
  ただし **DVP データ経路に ~1誤り/15-30KB のバイト誤り**（RGB ではごま塩で不可視・JPEG はリスタート
  マーカー無しのため最初の1誤りで以降全壊）で長い UXGA は化ける ── **この基板の信号品質限界**。クリーン UXGA は
  この DVP 経路では非現実的（smart-friend/codex とも一致。外部 FIFO 付きカメラが要る）。試した対策: Y モード
  （2 PCLK ごとに 1 バイトのサブサンプリングで不可）、FPIOA パッド調整（Schmitt 等／データ線は SPI0 共有で
  個別調整不可）、VGA JPEG（DVP フレーム同期せずハング）。
- **クロック/PLL 読み出し**（コミット `6c3723f`）: sysctl の PLL0/1/2・分周器レジスタをデコードして
  実周波数を算出（`PLLn = 26MHz/(clkr+1)*(clkf+1)/(clkod+1)`、`aclk=PLL0/2^(div+1)`）。実機で
  **PLL0=780MHz / CPU(aclk)=390MHz / APB=195MHz**。独立に UART 校正した CLINT mtime（=aclk/50）と
  クロスチェック → `7.80 vs 7.79 MHz, MATCH`。2 つの独立手法で CPU 周波数が裏取りできた。
- **マシンタイマ割り込み (ISR)**（コミット `14173c0`）: 初の割り込み駆動。CPU は `wfi` で寝て、CLINT
  マシンタイマ ISR が `mtimecmp` 再アーム・tick カウント・IO6(LED) トグル。riscv-rt は mtvec を張るだけ
  なので `mie.MTIE`/`mstatus.MIE` は生 CSR で自前有効化。出力 1 行＝割り込み 1 回、ホスト側の行間が
  **きっかり 0.500s（2 Hz）** で周期を実証。
- **FFT HW アクセラレータ（DMA 駆動）**（コミット `1e8b233`）: 実信号トーン
  `x[n]=10000·cos(2π·8n/64)` を 64 点 FFT → **bin 8（とミラー bin 56）にピーク**、振幅 5000・隣接 0 で `PASS`。
  **K210 FFT は MMIO データ経路を持たない**（FIFO への CPU 書き込みは握り潰される＝実機確認済み）ので、
  送信(RAM→入力FIFO)・受信(出力FIFO→RAM) の DMA 2 チャネルを同時に回して駆動。
- **DMAC ドライバ + メモリ間転送**（[src/dmac.rs](src/dmac.rs), コミット `b713365`）: DesignWare
  AXI DMA に HAL は無い（k210-hal の `dmac` は 32 行スタブ）ので PAC 直叩き。register sequence は唯一の
  完動 Rust 実装 [laanwj/k210-sdk-stuff](https://github.com/laanwj/k210-sdk-stuff) から移植し、現行の
  k210-hal/riscv-rt 0.11 に適合。K210 の癖: SRAM は 0x8000_0000 がキャッシュ有り／0x4000_0000 が無し
  別名なので、DMA バッファは**無し別名経由**で扱いコヒーレンシを確保。
- **AES-128 ECB HW アクセラレータ**（コミット `2b4f948`）: PAC 直叩き。FIPS-197 のテストベクタを
  暗号化 → 既知の `69c4e0d8...c55a` と照合し `PASS`。K210 の癖: `endian` レジスタを**鍵書き込みより
  先に**立てる（順序依存）、鍵は語順逆の LE、出力は LE。
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
