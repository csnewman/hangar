//! A PL011 UART, enough of one to be a console.
//!
//! The guest kernel is built with the PL011 driver and not the 16550, so the
//! same kernel image boots under this VMM and under QEMU's `virt` machine
//! without a configuration change.
//!
//! Only the console path is implemented: characters out, characters in, the
//! flag and interrupt registers the driver consults, and the PrimeCell
//! identification registers the AMBA bus reads to decide what this is.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::Arc;

use vmm_sys_util::eventfd::EventFd;

use super::MmioDevice;

const DR: u64 = 0x000;
const RSR_ECR: u64 = 0x004;
const FR: u64 = 0x018;
const IBRD: u64 = 0x024;
const FBRD: u64 = 0x028;
const LCR_H: u64 = 0x02c;
const CR: u64 = 0x030;
const IFLS: u64 = 0x034;
const IMSC: u64 = 0x038;
const RIS: u64 = 0x03c;
const MIS: u64 = 0x040;
const ICR: u64 = 0x044;
const DMACR: u64 = 0x048;
const ID_BASE: u64 = 0xfe0;

/// Flag register bits.
const FR_RXFE: u32 = 1 << 4; // receive FIFO empty
const FR_TXFF: u32 = 1 << 5; // transmit FIFO full
const FR_RXFF: u32 = 1 << 6; // receive FIFO full
const FR_TXFE: u32 = 1 << 7; // transmit FIFO empty

/// Interrupt bits, shared by IMSC, RIS, MIS and ICR.
const INT_RX: u32 = 1 << 4;
const INT_TX: u32 = 1 << 5;

/// PrimeCell identification. The first four are the PL011's peripheral ID,
/// `0x00341011`; the last four are the PrimeCell signature `0xb105f00d` that
/// every AMBA device carries.
const ID_REGISTERS: [u8; 8] = [0x11, 0x10, 0x34, 0x00, 0x0d, 0xf0, 0x05, 0xb1];

/// How many bytes of guest input to hold before dropping.
const RX_CAPACITY: usize = 256;

pub struct Pl011 {
    out: Box<dyn Write + Send>,
    irq: Arc<EventFd>,
    rx: VecDeque<u8>,

    /// Registers the driver writes and reads back. Their values change no
    /// behaviour here -- there is no real baud rate -- but a driver that
    /// cannot read back what it wrote decides the device is broken.
    ibrd: u32,
    fbrd: u32,
    lcr_h: u32,
    cr: u32,
    ifls: u32,
    dmacr: u32,

    imsc: u32,
    ris: u32,
}

impl Pl011 {
    pub fn new(out: Box<dyn Write + Send>, irq: Arc<EventFd>) -> Self {
        Self {
            out,
            irq,
            rx: VecDeque::new(),
            ibrd: 0,
            fbrd: 0,
            lcr_h: 0,
            // The driver expects the UART and its transmitter to be enabled
            // out of reset, as firmware would have left them.
            cr: 0x0301,
            ifls: 0x12,
            dmacr: 0,
            imsc: 0,
            ris: 0,
        }
    }

    /// Hand a byte of host input to the guest.
    pub fn enqueue_input(&mut self, byte: u8) {
        if self.rx.len() < RX_CAPACITY {
            self.rx.push_back(byte);
            self.raise(INT_RX);
        }
    }

    fn raise(&mut self, bits: u32) {
        self.ris |= bits;
        if self.ris & self.imsc != 0 {
            let _ = self.irq.write(1);
        }
    }

    fn flags(&self) -> u32 {
        let mut f = FR_TXFE;
        if self.rx.is_empty() {
            f |= FR_RXFE;
        } else if self.rx.len() >= RX_CAPACITY {
            f |= FR_RXFF;
        }
        // The transmit FIFO is never full: output goes straight to the host,
        // so there is nothing to fill. FR_TXFF stays clear.
        let _ = FR_TXFF;
        f
    }
}

impl MmioDevice for Pl011 {
    fn read(&mut self, offset: u64, data: &mut [u8]) {
        let value: u32 = match offset {
            DR => {
                let byte = self.rx.pop_front().unwrap_or(0);
                if self.rx.is_empty() {
                    self.ris &= !INT_RX;
                }
                u32::from(byte)
            }
            RSR_ECR => 0,
            FR => self.flags(),
            IBRD => self.ibrd,
            FBRD => self.fbrd,
            LCR_H => self.lcr_h,
            CR => self.cr,
            IFLS => self.ifls,
            IMSC => self.imsc,
            RIS => self.ris,
            MIS => self.ris & self.imsc,
            DMACR => self.dmacr,
            ID_BASE..=0xffc => {
                let i = ((offset - ID_BASE) / 4) as usize;
                ID_REGISTERS.get(i).copied().map_or(0, u32::from)
            }
            _ => 0,
        };
        write_le(data, value);
    }

    fn write(&mut self, offset: u64, data: &[u8]) {
        let value = read_le(data);
        match offset {
            DR => {
                let byte = [value as u8];
                // A console that cannot be written to is not worth failing a
                // guest over: the guest has no way to react, and the run
                // continues without its log.
                let _ = self.out.write_all(&byte);
                let _ = self.out.flush();
                self.raise(INT_TX);
            }
            RSR_ECR => {}
            IBRD => self.ibrd = value,
            FBRD => self.fbrd = value,
            LCR_H => self.lcr_h = value,
            CR => self.cr = value,
            IFLS => self.ifls = value,
            IMSC => {
                self.imsc = value;
                if self.ris & self.imsc != 0 {
                    let _ = self.irq.write(1);
                }
            }
            ICR => self.ris &= !value,
            DMACR => self.dmacr = value,
            _ => {}
        }
    }
}

fn read_le(data: &[u8]) -> u32 {
    let mut buf = [0u8; 4];
    let n = data.len().min(4);
    buf[..n].copy_from_slice(&data[..n]);
    u32::from_le_bytes(buf)
}

fn write_le(data: &mut [u8], value: u32) {
    let bytes = value.to_le_bytes();
    let n = data.len().min(4);
    data[..n].copy_from_slice(&bytes[..n]);
    if data.len() > 4 {
        data[4..].fill(0);
    }
}

/// Feed host input to the guest's console.
///
/// Only started when the console is on stdio: a run whose console goes to a
/// file has no terminal attached and nothing to read.
pub fn forward_stdin(uart: Arc<std::sync::Mutex<Pl011>>) {
    std::thread::Builder::new()
        .name("hangar-console-in".to_string())
        .spawn(move || {
            let mut buf = [0u8; 64];
            let mut stdin = io::stdin();
            loop {
                match io::Read::read(&mut stdin, &mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        let mut uart = uart.lock().unwrap();
                        for byte in &buf[..n] {
                            uart.enqueue_input(*byte);
                        }
                    }
                }
            }
        })
        .map(|_| ())
        .unwrap_or_else(|e| log::warn!("console input is unavailable: {e}"));
}

/// Where the guest's console output goes.
pub fn output(path: Option<&std::path::Path>) -> io::Result<Box<dyn Write + Send>> {
    match path {
        Some(p) => Ok(Box::new(std::fs::File::create(p)?)),
        None => Ok(Box::new(io::stdout())),
    }
}
