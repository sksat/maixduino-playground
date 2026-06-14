# 罠: rust-lld との格闘（と float ABI）

「コンパイルは通る」から「リンクが通る」までの間に、K210 で今の Rust を動かす固有の
リンク失敗が3つ立ちはだかった。順に:

## 1. riscv-rt 0.8 の `link.x` を今の rust-lld が拒否する

正典の [`riscv-rust/k210-example`](https://github.com/riscv-rust/k210-example) は
`riscv-rt = "0.8.0"` を固定している。その生成 linker script にはこうある:

```
(*(.trap));
```

GNU `ld` は冗長な外側の括弧を許すが、今の `rust-lld` は許さない:

```
rust-lld: error: .../riscv-rt-*/out/link.x:58: expected filename pattern
>>>     (*(.trap));
```

あの example が当時リンクできたのは、~2021 年の rust-lld が緩かったから。

**対処:** `riscv-rt = "0.11"` に上げる（その `link.x` は LLD クリーン）。`riscv` クレートは
直接使っていないので `[dependencies]` から外す。

## 2. float ABI 不一致: K210 は FPU を*持つ*のに、クレート群が soft-float

K210 のハードは **RV64IMAFDC (= RV64GC)** で、Kendryte データシート通り本物の IEEE-754
FPU を持つ —— 単精度*も*倍精度も。だから素直には `riscv64gc-unknown-none-elf`
（hard-float, `lp64d`）が target になる。すると:

```
rust-lld: error: libriscv-*.rlib(riscv.o): cannot link object files with
different floating-point ABI from .../symbols.o
```

`k210-hal` が引く**古い `riscv` クレートが soft-float (`lp64`) の*プリビルド* asm
オブジェクトを同梱**しており、rust-lld は float ABI の混在を拒否する。

**対処:** プリビルド asm に合わせて soft-float の
**`riscv64imac-unknown-none-elf`**（`lp64`）でビルドする。コードは整数演算のみなので
FPU を使わなくても損は無い。

> 余談: 公式 Kendryte C ツールチェーンは*第3の* ABI、
> `-march=rv64imafc -mabi=lp64f`（単精度 hard-float）を使う。Rust に
> `riscv64imafc` target は無いので、現実の選択肢は `imac`（soft）か `gc`（`lp64d`）。
> このクレート群でリンクが通るのは `imac` の方。

## 3. `.eh_frame` 再配置オーバーフロー（K210 が 0x80000000 ゆえの罠）

K210 SRAM は `0x8000_0000` —— ちょうど `2^31` —— にある。`libcore` は `.eh_frame` を
同梱しており、**riscv-rt 0.11 も最新の riscv-rt もこれを配置/破棄しない**。結果、
rust-lld はそれを低位アドレスの孤児セクションとして残す。その 32bit PC 相対再配置が
`.text`（`0x8000_0000`）を指すとオーバーフローする:

```
rust-lld: error: <internal>:(.eh_frame+0x...): relocation R_RISCV_32_PCREL out
of range: 2147489894 is not in [-2147483648, 2147483647]; references '.L0 '
```

多くの RISC-V ボードはコードを*低位*アドレスに置くので顕在化しない。K210 の高位 SRAM
ベースが引き金。こちらは `panic = "abort"` で unwinder が無く、`.eh_frame` は不要 ——
破棄する。

### 対処の中にもう一つ罠

rust-lld では、効きそうで**効かない**ものが2つ:

- 別スクリプトに書いた `/DISCARD/` を `INSERT AFTER <section>;` で足す方法
  —— rust-lld は overlay 側の discard を黙って無視する（エラーがバイト単位で同一なので
  スクリプトが無効だったと分かる）。
- `-C link-arg=--orphan-handling=discard` —— そんなモードは無い:
  `rust-lld: error: unknown --orphan-handling mode: discard`
  （有効なのは `place` / `warn` / `error` のみ）。

**効く**のは `memory.x` に discard を書くこと:

```
SECTIONS
{
  /DISCARD/ :
  {
    *(.eh_frame);
    *(.eh_frame_hdr);
  }
}
```

リンク行が `-Tmemory.x -Tlink.x` なので、2つの `SECTIONS` コマンドは**1つに連結**され、
`/DISCARD/` が honor される（`INSERT` overlay とは違う）。これで ELF はリンクが通り、
`.eh_frame` は消え、エントリは `0x80000000` になる。

## 最終的に repo に入っているもの

- target `riscv64imac-unknown-none-elf`（soft-float `lp64`）
- `riscv-rt = "0.11"`、`riscv` 直接依存なし
- `.eh_frame` は `memory.x` で破棄
- デフォルトの `rust-lld` でリンク —— **GNU binutils のインストール不要**

（もし GNU `ld` を使いたくなったら —— 例えば正典の riscv-rt 0.8 をそのまま使う等 ——
`riscv64-elf-binutils` は Arch 公式 `extra` にある。今回は repo 内/インストール不要を
保つため避けた。）
