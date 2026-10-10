//! Register transaction shared by the hardware adapter and fault-injection tests.
//! The caller serializes configuration access and keeps the device source off.

use super::{ApicMsiMessage, InterruptProgrammingError, MmioMapping, MsixCapability, PciDevice};
use core::sync::atomic::{fence, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Register {
    Control,
    AddressLow,
    AddressHigh,
    Data,
    Mask,
}

pub(super) trait InterruptIo {
    fn read(&mut self, register: Register) -> u32;
    fn write(&mut self, register: Register, value: u32);
}

#[derive(Clone, Copy)]
pub(super) struct Layout {
    pub msix: bool,
    pub address_high: bool,
    pub masking: bool,
}

impl Layout {
    fn disabled(self, control: u32) -> u32 {
        if self.msix {
            (control | (1 << 14)) & !(1 << 15)
        } else {
            control & !0x71 // Enable=0, MME=0; preserve MMC.
        }
    }

    fn enabled(self, control: u32) -> u32 {
        self.disabled(control) | if self.msix { 1 << 15 } else { 1 }
    }
}

fn checked_write(io: &mut impl InterruptIo, register: Register, value: u32) -> bool {
    io.write(register, value);
    // Read back from the device, also flushing posted MMIO writes.
    io.read(register) == value
}

/// Publish one message, verifying every write. On failure restore payload but
/// leave the capability disabled and the vector masked. No interrupt source
/// may be enabled by the caller until this transaction succeeds.
pub(super) fn program(
    io: &mut impl InterruptIo,
    layout: Layout,
    message: ApicMsiMessage,
) -> Result<(), InterruptProgrammingError> {
    use Register::*;
    let control = io.read(Control);
    if control & if layout.msix { 1 << 15 } else { 1 } != 0 {
        return Err(InterruptProgrammingError::AlreadyEnabled);
    }
    let low = io.read(AddressLow);
    let high = if layout.address_high {
        io.read(AddressHigh)
    } else {
        0
    };
    let data = io.read(Data);
    let mask = if layout.masking { io.read(Mask) } else { 0 };
    let programmed = (|| {
        if !checked_write(io, Control, layout.disabled(control))
            || (layout.masking && !checked_write(io, Mask, mask | 1))
            || !checked_write(io, AddressLow, message.address)
            || (layout.address_high && !checked_write(io, AddressHigh, 0))
            || !checked_write(io, Data, u32::from(message.data))
        {
            return false;
        }
        fence(Ordering::Release);
        if !checked_write(io, Control, layout.enabled(control))
            || (layout.masking && !checked_write(io, Mask, mask & !1))
        {
            return false;
        }
        !layout.msix || checked_write(io, Control, layout.enabled(control) & !(1 << 14))
    })();
    if programmed {
        return Ok(());
    }

    // Disable before restoring any payload; if that cannot be verified the
    // caller must retain ownership and must not try another interrupt mode.
    if !checked_write(io, Control, layout.disabled(control))
        || (layout.masking && !checked_write(io, Mask, mask | 1))
    {
        return Err(InterruptProgrammingError::RollbackFailed);
    }
    let mut restored = checked_write(io, AddressLow, low);
    if layout.address_high {
        restored &= checked_write(io, AddressHigh, high);
    }
    restored &= checked_write(io, Data, data);
    Err(if restored {
        InterruptProgrammingError::ReadbackFailed
    } else {
        InterruptProgrammingError::RollbackFailed
    })
}

/// Validate the entire table and PBA before entry zero is accessed. This first
/// consumer accepts a table in its already mapped BAR; no untrusted BIR causes
/// a fresh mapping. A PBA in another measured memory BAR need not be mapped.
pub(super) fn entry_address(
    device: PciDevice,
    mapping: MmioMapping,
    capability: MsixCapability,
) -> Result<u64, InterruptProgrammingError> {
    use InterruptProgrammingError::*;
    if !(1..=2048).contains(&capability.table_size) {
        return Err(InvalidCapability);
    }
    let table = device
        .bar_resource(capability.table_bir as usize)
        .filter(|bar| bar.bar.is_memory())
        .ok_or(InvalidBar)?;
    let pba = device
        .bar_resource(capability.pba_bir as usize)
        .filter(|bar| bar.bar.is_memory())
        .ok_or(InvalidBar)?;
    if table.base() != Some(mapping.physical_base) || table.size != mapping.size {
        return Err(InvalidBar);
    }
    let table_start = u64::from(capability.table_offset);
    let table_bytes = u64::from(capability.table_size) * 16;
    let table_end = table_start
        .checked_add(table_bytes)
        .ok_or(TableOutsideBar)?;
    let pba_start = u64::from(capability.pba_offset);
    let pba_end = pba_start
        .checked_add(u64::from(capability.table_size).div_ceil(64) * 8)
        .ok_or(TableOutsideBar)?;
    if table_start & 7 != 0
        || pba_start & 7 != 0
        || table_end > table.size
        || pba_end > pba.size
        || (table.index == pba.index && table_start < pba_end && pba_start < table_end)
    {
        return Err(TableOutsideBar);
    }
    mapping
        .virtual_address(table_start, table_bytes)
        .ok_or(TableOutsideBar)
}

#[cfg(test)]
mod tests {
    use super::super::{apic_msi_message, PciBar};
    use super::*;
    use std::vec::Vec;

    struct FakeIo {
        values: [u32; 5],
        writes: Vec<(Register, u32)>,
        fail_at: usize,
        fail_forever: bool,
        layout: Layout,
    }

    impl InterruptIo for FakeIo {
        fn read(&mut self, register: Register) -> u32 {
            self.values[register as usize]
        }
        fn write(&mut self, register: Register, value: u32) {
            if matches!(
                register,
                Register::AddressLow | Register::AddressHigh | Register::Data
            ) {
                assert_eq!(
                    self.values[0] & if self.layout.msix { 1 << 15 } else { 1 },
                    0
                );
                if self.layout.masking {
                    assert_eq!(self.values[4] & 1, 1);
                }
            }
            self.writes.push((register, value));
            if self.writes.len() == self.fail_at
                || (self.fail_forever && self.writes.len() >= self.fail_at)
            {
                return;
            }
            self.values[register as usize] = value;
        }
    }

    #[test]
    fn message_transactions_preserve_bits_and_rollback_each_failed_write() {
        for layout in [
            Layout {
                msix: true,
                address_high: true,
                masking: true,
            },
            Layout {
                msix: false,
                address_high: true,
                masking: true,
            },
            Layout {
                msix: false,
                address_high: false,
                masking: false,
            },
        ] {
            let original = [
                if layout.msix {
                    15
                } else {
                    0x180 | (3 << 1) | (2 << 4)
                },
                0xfee01000,
                1,
                0x45,
                0x10,
            ];
            let mut io = FakeIo {
                values: original,
                writes: Vec::new(),
                fail_at: 0,
                fail_forever: false,
                layout,
            };
            program(&mut io, layout, apic_msi_message(0x40, 0).unwrap()).unwrap();
            assert_eq!(io.values[1], 0xfee00000);
            assert_eq!(io.values[3], 0x40);
            assert_eq!(
                io.values[0],
                if layout.msix {
                    15 | (1 << 15)
                } else {
                    (original[0] & !0x70) | 1
                }
            );
            let writes = io.writes.len();
            for fail_at in 1..=writes {
                let mut faulty = FakeIo {
                    values: original,
                    writes: Vec::new(),
                    fail_at,
                    fail_forever: false,
                    layout,
                };
                assert_eq!(
                    program(&mut faulty, layout, apic_msi_message(0x40, 0).unwrap()),
                    Err(InterruptProgrammingError::ReadbackFailed)
                );
                assert_eq!(faulty.values[0], layout.disabled(original[0]));
                assert_eq!(&faulty.values[1..4], &original[1..4]);
                if layout.masking {
                    assert_eq!(faulty.values[4], original[4] | 1);
                }
            }
        }
    }

    #[test]
    fn rollback_failure_is_distinct_and_prevents_fallback() {
        let layout = Layout {
            msix: true,
            address_high: true,
            masking: true,
        };
        let mut io = FakeIo {
            values: [0, 0, 0, 0, 0],
            writes: Vec::new(),
            fail_at: 2,
            fail_forever: true,
            layout,
        };
        assert_eq!(
            program(&mut io, layout, apic_msi_message(0x40, 0).unwrap()),
            Err(InterruptProgrammingError::RollbackFailed)
        );
    }

    #[test]
    fn validates_full_msix_table_pba_bir_and_mapping() {
        let mut device = PciDevice::EMPTY;
        device.bars[0] = PciBar::Memory64 {
            base: 0x10000,
            prefetchable: false,
        };
        device.bar_sizes[0] = 0x4000;
        device.bars[1] = PciBar::UpperHalf64;
        device.bars[2] = PciBar::Io { base: 0x1000 };
        device.bar_sizes[2] = 0x4000;
        let mapping = MmioMapping::for_test(0x10000, 0x20000, 0x4000);
        let cap = MsixCapability {
            offset: 0x80,
            table_size: 16,
            table_bir: 0,
            table_offset: 0x3000,
            pba_bir: 0,
            pba_offset: 0x3800,
        };
        assert_eq!(entry_address(device, mapping, cap), Ok(0x23000));
        for bad in [
            MsixCapability {
                table_size: 0x200,
                ..cap
            },
            MsixCapability {
                pba_offset: 0x4000,
                ..cap
            },
            MsixCapability {
                pba_offset: 0x3000,
                ..cap
            },
            MsixCapability {
                table_bir: 1,
                ..cap
            },
            MsixCapability { pba_bir: 2, ..cap },
            MsixCapability {
                table_bir: 7,
                ..cap
            },
        ] {
            assert!(entry_address(device, mapping, bad).is_err());
        }
        assert!(
            entry_address(device, MmioMapping::for_test(0x50000, 0x20000, 0x4000), cap).is_err()
        );
    }
}
