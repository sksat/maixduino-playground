# オンボード LED は K210 IO6 だった（探索記）

結論: **Maixduino のオンボード LED（赤）は K210 の `IO6`、active-low。** GPIOHS で
普通に駆動できる。ここに辿り着くまでが長かったので記録する。

## 何が罠だったか

1. **ピン番号の二重の誤情報。**
   - Web 検索の「赤=IO13 / 緑=IO12 / 青=IO14」→ チップ IO で叩いても無反応。
   - Arduino core の `pins_arduino.h` を見ると、それらは *Arduino* ピン番号で、
     `_maixduino_pin_map[]` を通すと K210 IO は赤=IO3 / 緑=IO10 / 青=IO31 になる →
     これも無反応。
   - 実際は **IO6**。IO6 の FPIOA デフォルトは `RESV0`（未割当のフリーピン）で、
     まさにユーザ LED 向きのピンだった。

2. **readback が信用できなかった。** GPIOHS/GPIO の `input_val` / `data_input` で
   「パッドが駆動されているか」を測ろうとしたが、内部プルアップ入力でもプルダウン入力でも
   両方 `1`（`U1D1`）。input 系レジスタがパッドを反映せず、これに引っ張られて
   「GPIOHS は駆動できていない」と誤判断し、クロック/リセット/regular GPIO へと迷走した。
   **実際には GPIOHS は最初から正しく駆動できていて、ただピンが違っただけ。**

## 効いた方法: 総当たり → 二分探索

レジスタ readback が当てにならない以上、**唯一信用できる出力インジケータは LED 自体**。
そこで「人間の目」を使った:

1. `IO6..IO37` を GPIOHS0..31 に一括 mux し、**全部まとめて 1Hz 点滅** → 「点滅した！」
   （= GPIOHS は動く、LED はこの範囲）。
2. あとは駆動マスクを半分ずつにして二分探索:
   `IO6-21` → `IO6-13` → `IO6-9` → `IO6-7` → `IO6` で確定（5 回）。

ポイント: **GPIOHS は AHB クロック（CPU と同じ＝常時稼働）なので、PLL/クロック設定なしで
動く。** regular GPIO は APB0 上でクロック設定が要るぶん不利。最小構成では GPIOHS が楽。

## なぜドキュメント/Arduino と食い違うのか

公式 MaixPy チュートリアルも Arduino core も「赤=IO13 / 緑=IO12 / 青=IO14」と言う。
だが**この基板では一致しない**。念のため `input_en` をクリアした*正しい*出力設定で
チップ IO13/12/14 を再検証したが、それでも無反応（＝最初の失敗は input_en バグ「だけ」が
原因ではなく、本当に LED が無い）。整理すると:

- チップ IO13/12/14（直接）: 正しい設定でも無反応。
- IO3/10/31（Arduino map 変換後）: 無反応。
- IO6: 赤 LED 点灯。`_maixduino_pin_map[]` に「6」は無く、Arduino core はそもそも IO6 を
  指していない。

参照した Arduino core は正しい: `boards.txt` でボード `mduino`（Sipeed Maixduino）→
variant `sipeed_maixduino` で、Maixduino エントリは1つだけ（別リビジョン無し）。つまり
「間違った variant を見ていた」のではなく、**Maixduino 用 variant の LED 定義自体が実機と
食い違っている**。しかも該当箇所には `/* LEDs (USE Builtin TX PIN led)*/` という曰く
ありげなコメントがある。

最有力の説明: **「IO13/12/14 の RGB LED」は Maix Bit/Dock の配置**（これらは実際に
IO13/12/14 に RGB LED を持つ）**が Maixduino の定義にも流用された**、または基板
リビジョン差。確証には回路図が要るが、dl.sipeed.com の共有は Baidu/Mega 経由で直 DL 不可、
mouser の spec PDF も UA ブロックで取れず、WHY は推定どまり。確実なのは**実機の挙動 = IO6**。

## 確定した接続

| | |
|---|---|
| オンボード LED（赤） | K210 **IO6** |
| 論理 | **active-low**（Low で点灯） |
| 駆動 | FPIOA `IO6 -> GPIOHS0`、GPIOHS `output_en`/`output_val`（input_en はクリア） |

最終形は [`src/main.rs`](../src/main.rs)。
