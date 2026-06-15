//! Minimal K210 DMAC (Synopsys DesignWare AXI DMA) driver: enough for
//! single-block memory-to-memory and peripheral-handshake transfers.
//!
//! The register sequence is adapted from laanwj/k210-sdk-stuff
//! (`k210-shared/src/soc/dmac.rs`, ISC license) -- the only complete Rust DMAC
//! implementation around -- trimmed to what we use and ported to the k210-hal
//! git PAC and our riscv-rt 0.11 toolchain. The K210 FFT accelerator has no MMIO
//! data path (it only speaks to the DMA via TX/RX requests), so this driver is
//! what makes the FFT demo possible.

use k210_hal::pac;
use pac::dmac::channel::cfg::{HS_SEL_SRC_A, TT_FC_A};
use pac::dmac::channel::ctl::SMS_A;

pub use pac::dmac::channel::ctl::{
    SINC_A as AddressInc, SRC_MSIZE_A as BurstLen, SRC_TR_WIDTH_A as TransWidth,
};

/// K210 SRAM has a cached view at 0x8000_0000 and an uncached alias at
/// 0x4000_0000. DMA sees physical memory, so DMA buffers must be touched through
/// the uncached alias to stay coherent with the engine.
pub const UNCACHED_OFFSET: usize = 0x4000_0000;

/// Whether an address is RAM (software handshake) or a peripheral FIFO (hardware
/// handshake). Mirrors the Kendryte SDK's classification.
fn is_memory(address: u64) -> bool {
    let mem_len = 6 * 1024 * 1024;
    let mem_no_cache_len = 8 * 1024 * 1024;
    (address >= 0x8000_0000 && address < 0x8000_0000 + mem_len)
        || (address >= 0x4000_0000 && address < 0x4000_0000 + mem_no_cache_len)
        || (address == 0x5045_0040)
}

pub struct Dmac {
    dmac: pac::DMAC,
}

impl Dmac {
    pub fn new(dmac: pac::DMAC) -> Self {
        let d = Self { dmac };
        d.init();
        d
    }

    #[allow(dead_code)] // handy for bring-up / the mem-to-mem demo
    pub fn id(&self) -> u64 {
        self.dmac.id.read().bits()
    }
    #[allow(dead_code)] // used by the FFT demo
    pub fn version(&self) -> u64 {
        self.dmac.compver.read().bits()
    }

    fn init(&self) {
        unsafe {
            let sc = pac::SYSCTL::ptr();
            (*sc).clk_en_peri.modify(|_, w| w.dma_clk_en().set_bit());
        }

        self.dmac.reset.modify(|_, w| w.rst().set_bit());
        while self.dmac.reset.read().rst().bit() {}

        self.dmac.com_intclear.modify(|_, w| {
            w.slvif_dec_err()
                .set_bit()
                .slvif_wr2ro_err()
                .set_bit()
                .slvif_rd2wo_err()
                .set_bit()
                .slvif_wronhold_err()
                .set_bit()
                .slvif_undefinedreg_dec_err()
                .set_bit()
        });

        self.dmac
            .cfg
            .modify(|_, w| w.dmac_en().clear_bit().int_en().clear_bit());
        while self.dmac.cfg.read().bits() != 0 {}

        self.dmac.chen.modify(|_, w| {
            w.ch1_en()
                .clear_bit()
                .ch1_en_we()
                .set_bit()
                .ch2_en()
                .clear_bit()
                .ch2_en_we()
                .set_bit()
                .ch3_en()
                .clear_bit()
                .ch3_en_we()
                .set_bit()
                .ch4_en()
                .clear_bit()
                .ch4_en_we()
                .set_bit()
                .ch5_en()
                .clear_bit()
                .ch5_en_we()
                .set_bit()
        });

        self.dmac
            .cfg
            .modify(|_, w| w.dmac_en().set_bit().int_en().set_bit());
    }

    /// Route a hardware-handshake request line to a channel (sysctl dma_selN).
    /// `select` is a SYSCTL_DMA_SELECT_* value (e.g. FFT TX/RX request).
    #[allow(dead_code)] // used by the FFT demo
    pub fn set_select_request(&self, channel: usize, select: u8) {
        unsafe {
            let sc = pac::SYSCTL::ptr();
            match channel {
                0 => (*sc).dma_sel0.modify(|_, w| w.dma_sel0().bits(select)),
                1 => (*sc).dma_sel0.modify(|_, w| w.dma_sel1().bits(select)),
                2 => (*sc).dma_sel0.modify(|_, w| w.dma_sel2().bits(select)),
                3 => (*sc).dma_sel0.modify(|_, w| w.dma_sel3().bits(select)),
                4 => (*sc).dma_sel0.modify(|_, w| w.dma_sel4().bits(select)),
                _ => (*sc).dma_sel1.modify(|_, w| w.dma_sel5().bits(select)),
            }
        }
    }

    fn channel_enable(&self, ch: usize) {
        match ch {
            0 => self.dmac.chen.modify(|_, w| w.ch1_en().set_bit().ch1_en_we().set_bit()),
            1 => self.dmac.chen.modify(|_, w| w.ch2_en().set_bit().ch2_en_we().set_bit()),
            2 => self.dmac.chen.modify(|_, w| w.ch3_en().set_bit().ch3_en_we().set_bit()),
            3 => self.dmac.chen.modify(|_, w| w.ch4_en().set_bit().ch4_en_we().set_bit()),
            4 => self.dmac.chen.modify(|_, w| w.ch5_en().set_bit().ch5_en_we().set_bit()),
            _ => self.dmac.chen.modify(|_, w| w.ch6_en().set_bit().ch6_en_we().set_bit()),
        }
    }

    fn channel_disable(&self, ch: usize) {
        match ch {
            0 => self.dmac.chen.modify(|_, w| w.ch1_en().clear_bit().ch1_en_we().set_bit()),
            1 => self.dmac.chen.modify(|_, w| w.ch2_en().clear_bit().ch2_en_we().set_bit()),
            2 => self.dmac.chen.modify(|_, w| w.ch3_en().clear_bit().ch3_en_we().set_bit()),
            3 => self.dmac.chen.modify(|_, w| w.ch4_en().clear_bit().ch4_en_we().set_bit()),
            4 => self.dmac.chen.modify(|_, w| w.ch5_en().clear_bit().ch5_en_we().set_bit()),
            _ => self.dmac.chen.modify(|_, w| w.ch6_en().clear_bit().ch6_en_we().set_bit()),
        }
    }

    /// A channel's enable bit auto-clears when its block transfer completes.
    fn busy(&self, ch: usize) -> bool {
        let c = self.dmac.chen.read();
        match ch {
            0 => c.ch1_en().bit(),
            1 => c.ch2_en().bit(),
            2 => c.ch3_en().bit(),
            3 => c.ch4_en().bit(),
            4 => c.ch5_en().bit(),
            _ => c.ch6_en().bit(),
        }
    }

    fn interrupt_clear(&self, ch: usize) {
        unsafe {
            self.dmac.channel[ch].intclear.write(|w| w.bits(0xffff_ffff));
        }
    }

    fn set_channel_param(
        &self,
        ch: usize,
        src: u64,
        dst: u64,
        src_inc: AddressInc,
        dst_inc: AddressInc,
        burst: BurstLen,
        width: TransWidth,
        block_size: u32,
    ) {
        let src_mem = is_memory(src);
        let dst_mem = is_memory(dst);
        let flow = match (src_mem, dst_mem) {
            (false, false) => TT_FC_A::PRF2PRF_DMA,
            (true, false) => TT_FC_A::MEM2PRF_DMA,
            (false, true) => TT_FC_A::PRF2MEM_DMA,
            (true, true) => TT_FC_A::MEM2MEM_DMA,
        };
        let c = &self.dmac.channel[ch];

        // cfg must be written before block_ts/sar/dar.
        unsafe {
            c.cfg.modify(|_, w| {
                w.tt_fc()
                    .variant(flow)
                    .hs_sel_src()
                    .variant(if src_mem { HS_SEL_SRC_A::SOFTWARE } else { HS_SEL_SRC_A::HARDWARE })
                    .hs_sel_dst()
                    .variant(if dst_mem { HS_SEL_SRC_A::SOFTWARE } else { HS_SEL_SRC_A::HARDWARE })
                    .src_per()
                    .bits(ch as u8)
                    .dst_per()
                    .bits(ch as u8)
                    .src_multblk_type()
                    .bits(0)
                    .dst_multblk_type()
                    .bits(0)
            });

            c.sar.write(|w| w.bits(src));
            c.dar.write(|w| w.bits(dst));

            c.ctl.modify(|_, w| {
                w.sms()
                    .variant(SMS_A::AXI_MASTER_1)
                    .dms()
                    .variant(SMS_A::AXI_MASTER_2)
                    .sinc()
                    .variant(src_inc)
                    .dinc()
                    .variant(dst_inc)
                    .src_tr_width()
                    .variant(width)
                    .dst_tr_width()
                    .variant(width)
                    .src_msize()
                    .variant(burst)
                    .dst_msize()
                    .variant(burst)
            });

            c.block_ts.write(|w| w.block_ts().bits(block_size - 1));
        }
    }

    /// Start a single-block transfer and return immediately.
    pub fn start_single(
        &self,
        ch: usize,
        src: u64,
        dst: u64,
        src_inc: AddressInc,
        dst_inc: AddressInc,
        burst: BurstLen,
        width: TransWidth,
        block_size: u32,
    ) {
        self.interrupt_clear(ch);
        self.channel_disable(ch);
        self.wait_done(ch);
        self.set_channel_param(ch, src, dst, src_inc, dst_inc, burst, width, block_size);
        self.channel_enable(ch);
    }

    /// Spin until the channel's block transfer has finished.
    pub fn wait_done(&self, ch: usize) {
        while self.busy(ch) {}
        self.interrupt_clear(ch);
    }
}
