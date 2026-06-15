# ハードウェア: Maixduino (K210) 固有事項

データシートに載っているスペックではなく、コードを書くときに必要で*すぐには分からない*
配線・落とし穴だけ。

## LED（[hardware/](../hardware/) の回路図で確定）

**RGB ユーザ LED（LED_1615）= common-anode → +3V3、active-low**:

| 色 | K210 IO | 直列抵抗 |
|----|---------|----------|
| 赤 | IO13 | R31 4.7K |
| 緑 | IO12 | R32 10K |
| 青 | IO14 | R41 4.7K |

回路は正しい（3V3 → LED → 抵抗 → IO、IO を Low で点灯）。だが 4.7K で ~0.3mA と極端に暗く、
**実機で点灯を確認できていない**（暗すぎか個体死。詳細と全迷走は [finding-the-led.md](finding-the-led.md)）。

一方 **IO6（= `ESP32_U0TX` 線の活動 LED, 510R, ~3mA で明るい）は Rust でも MaixPy でも点滅する**。
これがユーザ LED と紛らわしいが、GPIO 制御自体は両スタックで動くことの確かな証拠。

注意:
- IO13/12/14 はカメラ DVP 信号と共有（IO13=DVP_HSYNC）だが、点灯しない件にカメラは無関係。
- Arduino core の `LED_RED=13` は Arduino ピン番号 → K210 IO3(=JTAG_TDO) を指すバグ。
  赤 LED を Arduino で叩くなら `digitalWrite(9, ...)`（= K210 IO13）。

## USB シリアル

オンボード USB-UART は K210 の高速 UART **UARTHS**、すなわち **IO5 = TX / IO4 = RX**
（MaixPy の REPL が使うのと同じピン）に配線されている。だから 115200 baud の `UARTHS`
出力が USB 経由でホストに出てくる —— `"Hello from Rust…"` / `LED on/off` が `picocom` に
届くのはこのため。

## 書き込み

`kflash` には **`-B maixduino`** プリセットがあり、ボードの自動 ISP リセット手順を
やってくれる。`cargo run`（`flash.sh` 経由）はこれを使う:

```
rust-objcopy -O binary <elf> <elf>.bin
uv run kflash -p /dev/ttyUSB0 -b 1500000 -B maixduino <elf>.bin
```

ハンドシェイクに失敗するなら baud を下げる（`K210_BAUD=460800 cargo run`）か
slow モードを試す。

## Arch でのシリアルポートアクセス（`dialout` ではなく `uucp`）

Arch Linux では `/dev/ttyUSB*` は **`uucp`** グループ所有（Debian 系の `dialout` は
存在しない）。一度だけ自分を追加して再ログイン:

```
sudo gpasswd -a "$USER" uucp
```

これをしないと `kflash` や `picocom` に `sudo` が要る。

## カメラ & WiFi —— 使えるが、既製の Rust HAL は無い

- **カメラ (OV2640/GC0328):** K210 の **DVP** パラレルインタフェース経由で KPU/AI
  パイプラインに入る。整理すると:
  - **HAL クレートには無い:** 使っている git 版 `k210-hal` に `dvp` モジュールは無い
    （aes/sha256/fft 等はあるが DVP は欠落）。
  - **PAC にはレジスタ定義が有る:** `k210-pac 0.2.0` には完全な `DVP` レジスタブロックが
    入っている（`dvp_cfg` / `r/g/b_addr` / `cmos_cfg` / `sccb_cfg`・`sccb_ctl`（センサ設定用
    の SCCB バス内蔵）/ `axi` / `sts` / `rgb_addr`）。AES/SHA256 と同じ **PAC 直叩き**で書ける。
  - **Rust の動作実装は既に有る:** [laanwj/k210-sdk-stuff](https://github.com/laanwj/k210-sdk-stuff)
    の `rust/dvp-ov` が OV2640 からフレームを読んで LCD に出す完動例で、`k210-shared` 内に
    `soc/dvp.rs`（DVP+SCCB ドライバ）と `board/ov2640.rs`（センサ設定）を持つ。割り込み/DMA
    未使用のポーリング実装。**ゼロから書く必要はなく、これを移植すればよい**。
- **WiFi (ESP32):** ESP32 はボード上の*別 MCU* で独自ファームを動かしており、K210 とは
  **SPI/UART** で通信する。Rust からはそのホスト側プロトコル（AT コマンド or Sipeed の
  SPI プロトコル）を `k210-hal` の SPI/シリアル上に自分で実装することになる。可能だが、
  import するものではなく自分で書くドライバ。

要するに: GPIO/UART/SPI/タイマは HAL で楽。カメラと WiFi は「PAC + 自前ドライバ」の領域。
[traps-crates-and-firmware.md](traps-crates-and-firmware.md) も参照。
