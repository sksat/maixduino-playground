# RGB LED を巡る調査（現状まとめ）

長時間ハマったので記録。**現時点の結論**:

- **ソフト（Rust も MaixPy も）は正しく動く。** K210 **IO6** の赤 LED は Rust でも公式 MaixPy
  でも点滅する。GPIO 制御は両スタックで機能している。
- **IO6** = 実在する controllable な赤 LED（明るい）。回路図では `ESP32_U0TX` 線の活動 LED で、
  直列 **510R**（~3mA）。ドキュメント上のユーザ LED ではないが、確実に光る。
- **IO13** = ドキュメント通りの **RGB 赤**。回路図でも確認: LED1(LED_1615) は **common-anode →
  +3V3**、R=IO13(R31 **4.7K**) / G=IO12(R32 10K) / B=IO14(R41 4.7K)、active-low。回路は正しい。
  だが Rust でも MaixPy でも**駆動しても視認できる点灯をしない**。
- 未決着: IO13 が **(a) ~0.3mA で暗すぎて見えない**（IO6 の 510R より約10倍暗い）のか、
  **(b) この個体の LED が死/未実装/断線**なのか。→ **テスタ実測待ち**（IO13 を Low 駆動中に
  R31 の両端に ~1.5V の電圧降下＝電流があるか）。

回路図・データシートは [`hardware/`](../hardware/) に確保（出典 dl.sipeed.com、Baidu/Mega 裏から救出）。

## 確定した事実

- **両ソフトスタックは動く**: IO6 を Rust(GPIOHS) でも MaixPy(regular GPIO) でも点滅できる。
  シリアル無しでも点滅する（＝純粋に GPIO 由来、UART 活動でも ESP32 の反応でもない）。
- MaixPy の readback で **IO13 のパッドは電気的に生きている**（pull-up→1 / pull-down→0）。
  何にも固定されていない（カメラ DVP も無関係と確認済み）。
- 公式 Arduino core の `LED_RED=13` は **Arduino ピン番号** → `_maixduino_pin_map` 経由で
  K210 **IO3 = JTAG_TDO**（LED ではない）を指す**バグ**。赤の Arduino ピンは **9**（= IO13）。
- 駆動方法は正しい: Rust(GPIOHS) は公式 Arduino core と同じ
  ([wiring_digital.c](https://github.com/sipeed/Maixduino/blob/master/cores/arduino/wiring_digital.c))。

## 迷走の教訓 — "光った" を肉眼の印象だけで信じない

ニセ陽性に何度も振り回された:
- **「MaixPy で点灯した」→ 実は ~70秒かかった MaixPy *書き込み中* の活動 LED** を見ていた
  （MaixPy 実行中ではない）。
- 「IO6 が一番明るく光る」に総当たり/二分探索が吸い寄せられ、一度「LED=IO6」と**誤結論**した
  （前版の本ドキュメント）。IO6 は確かに光る LED だが、回路図上の RGB ユーザ LED ではない。
- GPIO の `input_val`/`data_input` **readback が不正確**（プルアップ/ダウンで同値 `U1D1`）で、
  「駆動できてない」と誤判断 → クロック/リセット/PLL を延々追った。実際は出力は効いていた。

教訓: 暗い LED・活動 LED・書き込み中の点滅が全部ノイズになる。**確証は実測（テスタ）で。**
