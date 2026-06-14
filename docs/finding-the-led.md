# オンボード RGB LED を巡る長い迷走（未決着）

正直な現状: **Maixduino のオンボード RGB LED（K210 IO13=赤 / IO12=緑 / IO14=青）を、
実機で確実に点灯させられていない。** ソフトの問題か、ただ暗すぎて見えないのか、ハード固有
事情かは、客観測定（テスタ）待ち。長時間ハマったので記録する。

## 確定していること

- 回路図 [`hardware/Maixduino_2832_Schematic_v1.5.pdf`](../hardware/) と、ピンアサイン表
  をセル単位で確認: **K210 IO13=LED_R / IO12=LED_G / IO14=LED_B**、各 4.7K/10K 直列
  （= ~0.3mA で**極端に暗い**）。2020 年のデータシートも同じ。
- **IO6 = ESP32_U0TX**（K210↔ESP32 の UART 線）。LED ではない。
- Arduino core の `LED_RED=13` は **Arduino ピン番号**で、`_maixduino_pin_map` 経由で
  K210 **IO3 = JTAG_TDO**（LED ではない）を指す。つまり公式 `digitalWrite(LED_RED)` も
  実は赤 LED を叩かない。赤 LED の Arduino ピンは 9。
- 駆動方法は正しい: 公式 Arduino core も `pinMode`/`digitalWrite` で **GPIOHS** を使う
  （[wiring_digital.c](https://github.com/sipeed/Maixduino/blob/master/cores/arduino/wiring_digital.c)）。
  本リポジトリの Rust と同じ。MaixPy は regular GPIO を使う。どちらも試した。

## "光った" は全部ニセ陽性だった

迷走の主因。点滅して見えた光は、**すべて UART/ESP32 のアクティビティ LED** で、
RGB LED ではなかった:

- 「IO6 を叩いたら点滅」→ IO6=ESP32_TX を叩いたので **ESP32 が反応して自分の活動 LED**
  （510R で明るい）を点滅させただけ。
- 「MaixPy で点灯してた」→ 実は **MaixPy ファーム（717KB）の書き込みに ~70秒**かかり、
  その**書き込み中**の UART 活動 LED を見ていた（MaixPy 実行中ではない）。
- これらに引っ張られて、総当たり/二分探索が"一番明るく光るピン"（活動 LED）へ吸い寄せ
  られ、誤って「LED=IO6」と結論してしまった（前版の本ドキュメント）。**それは誤り。**

## 試して全部ダメだったこと（IO13 が視認できる形で点灯せず）

- GPIOHS / regular GPIO、両極性、`input_en` クリア、`gpio` リセット解除。
- カメラ有無（IO13=DVP_HSYNC でカメラと共有だが、A/B で**カメラは無関係**と確定）。
- MaixPy（regular GPIO）。さらに **MaixPy の動作中レジスタを `uctypes` で dump** し、
  `clk_en_cent=0x3f` / `clk_en_peri=0xecffffff` 等を Rust で丸ごと再現 → それでも視認できず。

## readback が当てにならない問題

GPIO の `input_val`/`data_input` でパッドを測ろうとしたが、プルアップ入力でもプルダウン
入力でも同値（`U1D1`）。input 系がパッドを反映せず、これにも振り回された。

## 残っている唯一の客観テスト

**テスタ/オシロで IO13 のピン電圧を測る。** 本リポジトリの `src/main.rs` は IO13 を
2秒周期で点滅させている:

- 0V↔3.3V で振れている → Rust は IO13 を駆動できている＝**暗すぎ/LED 不良で見えないだけ**
  （ソフト的には成功）。
- 固定で動かない → IO13 が駆動されていない＝さらに init（PLL/clk_sel 等）か別要因。

ここまでの教訓: **"光った" を肉眼の印象だけで信じない**（活動 LED・書き込み中の点滅・極端に
暗い LED が全部ノイズになる）。次は必ず実測で。
