# 罠: クレートとファームウェア

## crates.io の `k210-hal` は古いスタブ —— git を使う

crates.io の `k210-hal = "0.2.0"`（2019 公開）は機能を削ぎ落としたスナップショット:
`clock`・*非公開*の `fpioa`・`serial`・`stdout`・`time` だけで、**`gpio` も `gpiohs` も
`sysctl` も無い**。しかも `serial.configure()` は `pins` 引数を取る。本来の API で書くと
こう落ちる:

```
error[E0599]: no method named `constrain` found for struct `SYSCTL`
error[E0599]: no method named `split` found for struct `FPIOA`
error[E0061]: this method takes 3 arguments but 2 arguments were supplied   (configure)
```

**本来の** API —— `SYSCTL.constrain()`・`FPIOA.split()`・`into_function()`、そして
`gpio`/`gpiohs`/`spi`/`dmac`/`fft`/`aes`/`sha256`/`apu` のモジュール —— は
**git `master`** に*同じ*バージョン番号 `0.2.0` のまま存在し、再公開されていない。正典の
`k210-example` の `Cargo.toml` も `0.2.0` と書いてあるのにコードは master の API を使う
—— つまりあれも crates.io ではなく git に対してビルドしている。

**対処:** `k210-hal = { git = "https://github.com/riscv-rust/k210-hal" }`
（Cargo.lock がコミットを固定する —— 執筆時点で `ff202db`）。

### …しかも master の GPIO HAL は未完成

`gpiohs.rs` は**チャネル 0 だけ**、しかも*入力*コンストラクタだけ:

```rust
pub struct Parts { pub gpiohs0: Gpiohs0<Input<Floating>> }
// impl<MODE> Gpiohs0<MODE> { pub fn into_pull_up_input(...) ...  // todo: all modes
```

`OutputPin` は `Gpiohs0<Output<_>>` に `impl` されているが、**`Output` を安全に作る手段が
無い** —— `into_push_pull_output()` が存在しない。なので LED 駆動は PAC に降りて
`GPIOHS.output_en` / `GPIOHS.output_val` を直接叩く（`src/main.rs` 参照）。「HAL がある」
≠「その HAL が対応している」という良い教訓。

## 「MaixPy v4」は K210 用ではない

MicroPython ファームを探すなら注意: 今の
[`sipeed/MaixPy` releases](https://github.com/sipeed/MaixPy/releases) は **MaixPy v4.x、
ターゲットは MaixCAM**（SG2002 / CV181x）という*別チップ*。Maixduino (K210) に焼くのは
誤りで、リリースノートも「間違ったイメージを焼くな」と明記している。

**K210** の MicroPython ファームは旧 **MaixPy v0.6.x** 系列。今回は MaixPy を使わず
ベアメタル Rust に行ったが、サッと REPL したい人がよく踏む罠。

## Rust から K210 の何を叩けるか

このクレート群でおおよそ:

- **HAL レベル（`k210-hal` master）:** clocks/`sysctl`、`fpioa` ピンmux、UART/UARTHS、
  SPI、GPIO/GPIOHS（一部 —— 上記）、DMAC、タイマ/`clint`、`plic`、そして
  アクセラレータの `aes`/`sha256`/`fft`/`apu`。
- **PAC レベル（`k210-pac`）:** 全ペリフェラルの全レジスタ。HAL の無いものも含む。
- **穴:** **KPU/NPU**（AI アクセラレータ）や **DVP カメラ**の成熟した HAL は無い ——
  ここは C SDK がまだ先行。PAC + `unsafe` で叩けるが自力。
