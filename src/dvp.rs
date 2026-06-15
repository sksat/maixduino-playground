//! K210 DVP (Digital Video Port) + SCCB driver, enough to talk to an OV2640 over
//! SCCB and capture frames. There is no DVP HAL (k210-hal has no dvp module), but
//! the k210-pac DVP register block is complete. Ported from laanwj/k210-sdk-stuff
//! (`k210-shared/src/soc/dvp.rs`, ISC), with its sysctl helpers replaced by raw
//! register writes to fit our k210-hal/riscv-rt 0.11 toolchain.
//!
//! The DVP is its own AXI bus master: it writes captured frames straight to the
//! addresses in r/g/b_addr (planar, for the KPU) or rgb_addr (RGB565, packed), no
//! DMA needed. SCCB is the OV2640's I2C-like config bus, built into the DVP.

#![allow(dead_code)] // frame-capture methods are used by the next demo step

use k210_hal::pac;

pub use pac::dvp::dvp_cfg::FORMAT_A as ImageFormat;

/// Rough busy-wait microsecond delay (overshoots; only used for sensor power/reset
/// pulses where "at least this long" is all that matters). CPU ~390 MHz.
fn usleep(us: u32) {
    for _ in 0..us.saturating_mul(60) {
        unsafe { core::arch::asm!("nop") };
    }
}

pub struct Dvp {
    dvp: pac::DVP,
}

impl Dvp {
    pub fn new(dvp: pac::DVP) -> Self {
        Self { dvp }
    }

    /// SCCB clock as slow/safe as possible (max divider).
    fn sccb_clk_init(&self) {
        unsafe {
            self.dvp
                .sccb_cfg
                .modify(|_, w| w.scl_lcnt().bits(255).scl_hcnt().bits(255));
        }
    }

    fn sccb_start_transfer(&self) {
        while self.dvp.sts.read().sccb_en().bit() {}
        self.dvp
            .sts
            .write(|w| w.sccb_en().set_bit().sccb_en_we().set_bit());
        while self.dvp.sts.read().sccb_en().bit() {}
    }

    /// Write one 8-bit register (8-bit reg address) over SCCB.
    pub fn sccb_send(&self, dev_addr: u8, reg_addr: u8, reg_data: u8) {
        use pac::dvp::sccb_cfg::BYTE_NUM_A::NUM3;
        unsafe {
            self.dvp.sccb_cfg.modify(|_, w| w.byte_num().variant(NUM3));
            self.dvp.sccb_ctl.write(|w| {
                w.device_address()
                    .bits(dev_addr | 1)
                    .reg_address()
                    .bits(reg_addr)
                    .wdata_byte0()
                    .bits(reg_data)
            });
        }
        self.sccb_start_transfer();
    }

    /// Read one 8-bit register (8-bit reg address) over SCCB.
    pub fn sccb_receive(&self, dev_addr: u8, reg_addr: u8) -> u8 {
        use pac::dvp::sccb_cfg::BYTE_NUM_A::NUM2;
        unsafe {
            self.dvp.sccb_cfg.modify(|_, w| w.byte_num().variant(NUM2));
            self.dvp
                .sccb_ctl
                .write(|w| w.device_address().bits(dev_addr | 1).reg_address().bits(reg_addr));
        }
        self.sccb_start_transfer();
        unsafe {
            self.dvp.sccb_ctl.write(|w| w.device_address().bits(dev_addr));
        }
        self.sccb_start_transfer();
        self.dvp.sccb_cfg.read().rdata().bits()
    }

    /// Power-cycle and reset the attached sensor.
    fn reset_sensor(&self) {
        self.dvp.cmos_cfg.modify(|_, w| w.power_down().set_bit());
        usleep(2000);
        self.dvp.cmos_cfg.modify(|_, w| w.power_down().clear_bit());
        usleep(2000);
        self.dvp.cmos_cfg.modify(|_, w| w.reset().clear_bit());
        usleep(2000);
        self.dvp.cmos_cfg.modify(|_, w| w.reset().set_bit());
        usleep(2000);
    }

    /// Enable DVP clock, reset it, start XCLK (~APB1/8 ≈ 24 MHz), init SCCB, reset
    /// the sensor.
    pub fn init(&self) {
        unsafe {
            let sc = pac::SYSCTL::ptr();
            (*sc).clk_en_peri.modify(|_, w| w.dvp_clk_en().set_bit());
            (*sc).peri_reset.modify(|_, w| w.dvp_reset().set_bit());
            usleep(10);
            (*sc).peri_reset.modify(|_, w| w.dvp_reset().clear_bit());
            self.dvp
                .cmos_cfg
                .modify(|_, w| w.clk_div().bits(3).clk_enable().set_bit());
        }
        self.sccb_clk_init();
        self.reset_sensor();
    }

    pub fn set_image_format(&self, format: ImageFormat) {
        self.dvp.dvp_cfg.modify(|_, w| w.format().variant(format));
    }

    /// Set frame size. With burst mode, width must be a multiple of 32.
    pub fn set_image_size(&self, burst_mode: bool, width: u16, height: u16) {
        use pac::dvp::axi::GM_MLEN_A;
        let burst_num = if burst_mode {
            self.dvp.dvp_cfg.modify(|_, w| w.burst_size_4beats().set_bit());
            self.dvp.axi.modify(|_, w| w.gm_mlen().variant(GM_MLEN_A::BYTE4));
            width / 8 / 4
        } else {
            self.dvp.dvp_cfg.modify(|_, w| w.burst_size_4beats().clear_bit());
            self.dvp.axi.modify(|_, w| w.gm_mlen().variant(GM_MLEN_A::BYTE1));
            width / 8
        };
        unsafe {
            self.dvp
                .dvp_cfg
                .modify(|_, w| w.href_burst_num().bits(burst_num as u8).line_num().bits(height));
        }
    }

    /// Planar R8G8B8 output (for the KPU, but also plain memory). `None` disables.
    pub fn set_ai_addr(&self, addr: Option<(u32, u32, u32)>) {
        if let Some((r, g, b)) = addr {
            unsafe {
                self.dvp.r_addr.write(|w| w.bits(r));
                self.dvp.g_addr.write(|w| w.bits(g));
                self.dvp.b_addr.write(|w| w.bits(b));
            }
            self.dvp.dvp_cfg.modify(|_, w| w.ai_output_enable().set_bit());
        } else {
            self.dvp.dvp_cfg.modify(|_, w| w.ai_output_enable().clear_bit());
        }
    }

    /// Packed RGB565 output. `None` disables.
    pub fn set_display_addr(&self, addr: Option<u32>) {
        if let Some(a) = addr {
            unsafe {
                self.dvp.rgb_addr.write(|w| w.bits(a));
            }
            self.dvp.dvp_cfg.modify(|_, w| w.display_output_enable().set_bit());
        } else {
            self.dvp.dvp_cfg.modify(|_, w| w.display_output_enable().clear_bit());
        }
    }

    pub fn set_auto(&self, enable: bool) {
        self.dvp.dvp_cfg.modify(|_, w| w.auto_enable().bit(enable));
    }

    /// Capture one full frame (blocking).
    pub fn get_image(&self) {
        while !self.dvp.sts.read().frame_start().bit() {}
        self.dvp
            .sts
            .write(|w| w.frame_start().set_bit().frame_start_we().set_bit());
        while !self.dvp.sts.read().frame_start().bit() {}
        self.dvp.sts.write(|w| {
            w.frame_finish()
                .set_bit()
                .frame_finish_we()
                .set_bit()
                .frame_start()
                .set_bit()
                .frame_start_we()
                .set_bit()
                .dvp_en()
                .set_bit()
                .dvp_en_we()
                .set_bit()
        });
        while !self.dvp.sts.read().frame_finish().bit() {}
    }
}

/// OV2640 SCCB device address on the K210 DVP.
pub const OV2640_ADDR: u8 = 0x60;

/// Read (manufacturer_id, product_id). Expect (0x7fa2, 0x2642) for OV2640.
pub fn ov2640_read_id(dvp: &Dvp) -> (u16, u16) {
    dvp.sccb_send(OV2640_ADDR, 0xff, 0x01); // bank select: sensor
    let manuf = (u16::from(dvp.sccb_receive(OV2640_ADDR, 0x1c)) << 8)
        | u16::from(dvp.sccb_receive(OV2640_ADDR, 0x1d));
    let pid = (u16::from(dvp.sccb_receive(OV2640_ADDR, 0x0a)) << 8)
        | u16::from(dvp.sccb_receive(OV2640_ADDR, 0x0b));
    (manuf, pid)
}
