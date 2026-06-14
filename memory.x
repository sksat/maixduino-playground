MEMORY
{
  /* K210 has 8 MiB of on-chip SRAM at 0x80000000:
       - 6 MiB general SRAM (SRAM0)  0x80000000 .. 0x80600000
       - 2 MiB AI/KPU SRAM (SRAM1)   0x80600000 .. 0x80800000
     We run entirely out of the 6 MiB general region. kflash loads the image
     here and the ROM boots into it. */
  SRAM : ORIGIN = 0x80000000, LENGTH = 6M
}

REGION_ALIAS("REGION_TEXT", SRAM);
REGION_ALIAS("REGION_RODATA", SRAM);
REGION_ALIAS("REGION_DATA", SRAM);
REGION_ALIAS("REGION_BSS", SRAM);
REGION_ALIAS("REGION_HEAP", SRAM);
REGION_ALIAS("REGION_STACK", SRAM);

_stack_start = ORIGIN(SRAM) + LENGTH(SRAM);

/* K210 SRAM is at 0x8000_0000 (= 2^31). libcore ships `.eh_frame`, which
   riscv-rt's link.x never places; rust-lld then drops it as an orphan at a low
   address and its 32-bit PC-relative relocations into .text overflow 2 GiB.
   We're panic=abort with no unwinder, so discard it. This SECTIONS is
   concatenated ahead of link.x's into one SECTIONS, so unlike an INSERT overlay
   (which rust-lld ignores) the /DISCARD/ is honored. */
SECTIONS
{
  /DISCARD/ :
  {
    *(.eh_frame);
    *(.eh_frame_hdr);
  }
}
