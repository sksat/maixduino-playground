# ハードウェア: Maixduino (K210) 固有事項

データシートに載っているスペックではなく、コードを書くときに必要で*すぐには分からない*
配線・落とし穴だけ。

## オンボード LED

3つ、すべて**アクティブLow**（ピンを **Low** に駆動すると点灯）:

| 色   | K210 IO |
|------|---------|
| 赤   | IO13    |
| 緑   | IO12    |
| 青   | IO14    |

本プロジェクトは**赤**を点滅させる。K210 では FPIOA により任意の IO を任意のペリフェラルに
ルーティングできるので、`IO13 -> GPIOHS0` に mux して GPIOHS チャネル 0 を駆動する。

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
  パイプラインに入る。**`k210-hal` に DVP ドライバは無い** —— `k210-pac` のレジスタを
  自分で叩く必要がある（C SDK や MaixPy にはあるが、Rust にはまだ無い）。
- **WiFi (ESP32):** ESP32 はボード上の*別 MCU* で独自ファームを動かしており、K210 とは
  **SPI/UART** で通信する。Rust からはそのホスト側プロトコル（AT コマンド or Sipeed の
  SPI プロトコル）を `k210-hal` の SPI/シリアル上に自分で実装することになる。可能だが、
  import するものではなく自分で書くドライバ。

要するに: GPIO/UART/SPI/タイマは HAL で楽。カメラと WiFi は「PAC + 自前ドライバ」の領域。
[traps-crates-and-firmware.md](traps-crates-and-firmware.md) も参照。
