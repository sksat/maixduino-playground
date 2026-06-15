//! K210 real-time clock (RTC), PAC-direct. There is no RTC HAL (k210-hal has no
//! `rtc` module), but `k210-pac` has the full register block. Ported from the
//! kendryte-standalone-sdk `rtc.c` sequence.
//!
//! The RTC counts its input clock (the 26 MHz crystal, IN0): when `current_count`
//! reaches `initial_count` one second elapses and date/time advance. So
//! `initial_count = 26_000_000` gives a 1 Hz wall clock. `register_ctrl` gates
//! access: `write_enable` to write the date/time/count registers ("setting"),
//! `read_enable` so reads reflect the live counter ("running"); the `*_mask` bits
//! must be un-masked or writes are dropped.

#![allow(dead_code)]

use k210_hal::pac;

/// Input clock to the RTC (the 26 MHz crystal); dividing by it yields 1 Hz.
const RTC_CLOCK_HZ: u32 = 26_000_000;

pub struct Rtc {
    rtc: pac::RTC,
}

impl Rtc {
    pub fn new(rtc: pac::RTC) -> Self {
        Self { rtc }
    }

    fn set_setting(&self) {
        // writable: write_enable=1, read_enable=0
        self.rtc
            .register_ctrl
            .modify(|_, w| w.read_enable().clear_bit().write_enable().set_bit());
    }
    fn set_running(&self) {
        // readable & live: read_enable=1, write_enable=0
        self.rtc
            .register_ctrl
            .modify(|_, w| w.read_enable().set_bit().write_enable().clear_bit());
    }

    /// Clock + reset the RTC, un-protect the registers, program the 1 Hz divider.
    pub fn init(&self) {
        unsafe {
            let sc = pac::SYSCTL::ptr();
            (*sc).peri_reset.modify(|_, w| w.rtc_reset().set_bit());
            for _ in 0..10_000 {
                core::arch::asm!("nop");
            }
            (*sc).peri_reset.modify(|_, w| w.rtc_reset().clear_bit());
            (*sc).clk_en_peri.modify(|_, w| w.rtc_clk_en().set_bit());
        }

        // Un-mask everything and enter setting mode in one write.
        self.rtc.register_ctrl.modify(|_, w| unsafe {
            w.read_enable()
                .clear_bit()
                .write_enable()
                .set_bit()
                .timer_mask()
                .bits(0xff)
                .alarm_mask()
                .bits(0xff)
                .initial_count_mask()
                .set_bit()
                .interrupt_register_mask()
                .set_bit()
        });

        unsafe {
            self.rtc.initial_count.write(|w| w.bits(RTC_CLOCK_HZ)); // ticks per second
            self.rtc.current_count.write(|w| w.bits(1));
        }
    }

    /// Set the wall-clock date/time.
    pub fn set_datetime(
        &self,
        year: u16,
        month: u8,
        day: u8,
        week: u8,
        hour: u8,
        minute: u8,
        second: u8,
    ) {
        self.set_setting();
        unsafe {
            self.rtc
                .date
                .write(|w| w.year().bits(year).month().bits(month).day().bits(day).week().bits(week));
            self.rtc
                .time
                .write(|w| w.hour().bits(hour).minute().bits(minute).second().bits(second));
        }
        self.set_running();
    }

    /// (hour, minute, second) from the live counter.
    pub fn time(&self) -> (u8, u8, u8) {
        let t = self.rtc.time.read();
        (t.hour().bits(), t.minute().bits(), t.second().bits())
    }

    /// (year, month, day) from the live counter.
    pub fn date(&self) -> (u16, u8, u8) {
        let d = self.rtc.date.read();
        (d.year().bits(), d.month().bits(), d.day().bits())
    }
}
