#![allow(dead_code)]
// ============================================================
// Brane OS Kernel — Page Table Manager
// ============================================================
//
// Manages virtual-to-physical address mappings using x86_64
// 4-level page tables.
//
// Provides:
//   - Active page table access (via physical memory offset)
//   - Map / unmap operations
//   - Boot-info aware initialization
//
// Spec reference: ARCHITECTURE.md §5.2.3
// ============================================================

use x86_64::registers::control::Cr3;
use x86_64::structures::paging::page_table::PageTableEntry;
use x86_64::structures::paging::{
    FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame, Size4KiB,
    Translate,
};
use x86_64::{PhysAddr, VirtAddr};

use crate::syscall::{UserPagePermissions, UserPageValidationError, UserRange};

// -----------------------------------------------------------------------
// Initialization
// -----------------------------------------------------------------------

/// Initialize an `OffsetPageTable` from the currently active level-4 table.
///
/// # Safety
/// - `physical_memory_offset` must be correct (all physical memory is
///   mapped at this virtual offset by the bootloader).
/// - Must only be called once to avoid aliasing `&mut` references.
pub unsafe fn init(physical_memory_offset: VirtAddr) -> OffsetPageTable<'static> {
    let level_4_table = active_level_4_table(physical_memory_offset);
    unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) }
}

/// Get a mutable reference to the active level-4 page table.
unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    let (level_4_frame, _) = Cr3::read();
    let phys = level_4_frame.start_address();
    let virt = physical_memory_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();
    unsafe { &mut *page_table_ptr }
}

// -----------------------------------------------------------------------
// Mapping helpers
// -----------------------------------------------------------------------

/// Map a virtual page to a physical frame.
///
/// Uses the provided frame allocator for any intermediate page table frames.
pub fn map_page(
    mapper: &mut OffsetPageTable,
    page: Page<Size4KiB>,
    frame: PhysFrame<Size4KiB>,
    flags: PageTableFlags,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), &'static str> {
    unsafe {
        mapper
            .map_to(page, frame, flags, frame_allocator)
            .map_err(|_| "map_to failed")?
            .flush();
    }
    Ok(())
}

/// Map a range of virtual pages to physical frames (identity or offset).
pub fn map_range(
    mapper: &mut OffsetPageTable,
    start_page: Page<Size4KiB>,
    start_frame: PhysFrame<Size4KiB>,
    count: u64,
    flags: PageTableFlags,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), &'static str> {
    for i in 0..count {
        let page = Page::containing_address(start_page.start_address() + i * 4096);
        let frame = PhysFrame::containing_address(start_frame.start_address() + i * 4096);
        map_page(mapper, page, frame, flags, frame_allocator)?;
    }
    Ok(())
}

/// Unmap a virtual page and return the physical frame it was mapped to.
pub fn unmap_page(
    mapper: &mut OffsetPageTable,
    page: Page<Size4KiB>,
) -> Result<PhysFrame<Size4KiB>, &'static str> {
    let (frame, flush) = mapper.unmap(page).map_err(|_| "unmap failed")?;
    flush.flush();
    Ok(frame)
}

// -----------------------------------------------------------------------
// Info / debugging
// -----------------------------------------------------------------------

/// Translate a virtual address to a physical address.
pub fn translate_addr(mapper: &OffsetPageTable, addr: VirtAddr) -> Option<PhysAddr> {
    mapper.translate_addr(addr)
}

// -----------------------------------------------------------------------
// User mapping validation
// -----------------------------------------------------------------------

/// Validate the mappings and effective permissions for every page in a
/// user range.
///
/// The walk intersects flags from every level of the hierarchy.  x86 applies
/// `USER_ACCESSIBLE` and `WRITABLE` from parent entries to the whole subtree;
/// checking only the leaf entry would therefore be insufficient.  The caller
/// must hold the address-space pin/read lock while validating and copying;
/// this function intentionally does not invent a global mapper lock.
pub fn validate_user_range(
    mapper: &OffsetPageTable,
    range: UserRange,
) -> Result<(), UserPageValidationError> {
    range.validate_pages(|page_start| effective_user_page_permissions(mapper, page_start))
}

/// Read the effective user permissions for one page without touching its
/// contents.  The page-table hierarchy itself is reachable through the
/// physical-memory offset held by `OffsetPageTable`.
fn effective_user_page_permissions(
    mapper: &OffsetPageTable,
    page_start: u64,
) -> UserPagePermissions {
    let page = Page::<Size4KiB>::containing_address(VirtAddr::new(page_start));
    let p4_entry = &mapper.level_4_table()[page.p4_index()];
    let p4_flags = p4_entry.flags();
    if !p4_flags.contains(PageTableFlags::PRESENT) {
        return UserPagePermissions::unmapped();
    }

    let p3 = match unsafe { child_table(mapper, p4_entry) } {
        Some(table) => table,
        None => return UserPagePermissions::unmapped(),
    };
    let p3_entry = &p3[page.p3_index()];
    let p3_flags = p3_entry.flags();
    if !p3_flags.contains(PageTableFlags::PRESENT) {
        return UserPagePermissions::unmapped();
    }
    if p3_flags.contains(PageTableFlags::HUGE_PAGE) {
        // Bit 12 is PAT for a large-page leaf, not part of its frame base.
        if p3_entry.addr().as_u64() & (((1 << 30) - 1) & !(1 << 12)) != 0 {
            return UserPagePermissions::unmapped();
        }
        return permissions_from_flags(intersect_flags(p4_flags, p3_flags));
    }

    let p2 = match unsafe { child_table(mapper, p3_entry) } {
        Some(table) => table,
        None => return UserPagePermissions::unmapped(),
    };
    let p2_entry = &p2[page.p2_index()];
    let p2_flags = p2_entry.flags();
    if !p2_flags.contains(PageTableFlags::PRESENT) {
        return UserPagePermissions::unmapped();
    }
    if p2_flags.contains(PageTableFlags::HUGE_PAGE) {
        if p2_entry.addr().as_u64() & (((1 << 21) - 1) & !(1 << 12)) != 0 {
            return UserPagePermissions::unmapped();
        }
        return permissions_from_flags(intersect_flags(
            intersect_flags(p4_flags, p3_flags),
            p2_flags,
        ));
    }

    let p1 = match unsafe { child_table(mapper, p2_entry) } {
        Some(table) => table,
        None => return UserPagePermissions::unmapped(),
    };
    let p1_flags = p1[page.p1_index()].flags();
    if !p1_flags.contains(PageTableFlags::PRESENT) {
        return UserPagePermissions::unmapped();
    }

    permissions_from_flags(intersect_flags(
        intersect_flags(intersect_flags(p4_flags, p3_flags), p2_flags),
        p1_flags,
    ))
}

fn permissions_from_flags(flags: PageTableFlags) -> UserPagePermissions {
    UserPagePermissions {
        present: flags.contains(PageTableFlags::PRESENT),
        user_accessible: flags.contains(PageTableFlags::USER_ACCESSIBLE),
        writable: flags.contains(PageTableFlags::WRITABLE),
    }
}

fn intersect_flags(left: PageTableFlags, right: PageTableFlags) -> PageTableFlags {
    PageTableFlags::from_bits_retain(left.bits() & right.bits())
}

/// Resolve a non-leaf page-table entry through the mapper's direct map.
///
/// # Safety
/// `OffsetPageTable` promises that its physical-memory offset maps all page
/// table frames.  The function only returns entries marked present and not
/// huge, so the pointed-to frame is a child page table.
unsafe fn child_table<'a>(
    mapper: &'a OffsetPageTable,
    entry: &PageTableEntry,
) -> Option<&'a PageTable> {
    let flags = entry.flags();
    if !flags.contains(PageTableFlags::PRESENT) || flags.contains(PageTableFlags::HUGE_PAGE) {
        return None;
    }
    let table_address = mapper
        .phys_offset()
        .as_u64()
        .checked_add(entry.addr().as_u64())?;
    let table_virtual = VirtAddr::try_new(table_address).ok()?;
    Some(unsafe { &*table_virtual.as_ptr::<PageTable>() })
}

#[cfg(test)]
mod user_mapping_tests {
    use super::*;
    use crate::syscall::UserAccess;
    use std::boxed::Box;

    const START: u64 = 0x0040_0000;

    fn user_flags() -> PageTableFlags {
        PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::WRITABLE
    }

    // With offset zero, host addresses stand in for physical page-table
    // frames. Only these owned tables are dereferenced; leaf data is never
    // accessed and no privileged CPU instruction is executed.
    struct Tables {
        p4: Box<PageTable>,
        p3: Box<PageTable>,
        p2: Box<PageTable>,
        p1: Box<PageTable>,
    }

    impl Tables {
        fn new() -> Self {
            let mut tables = Self {
                p4: Box::new(PageTable::new()),
                p3: Box::new(PageTable::new()),
                p2: Box::new(PageTable::new()),
                p1: Box::new(PageTable::new()),
            };
            tables.p4[0].set_addr(
                PhysAddr::new((&*tables.p3 as *const PageTable) as u64),
                user_flags(),
            );
            tables.p3[0].set_addr(
                PhysAddr::new((&*tables.p2 as *const PageTable) as u64),
                user_flags(),
            );
            tables.p2[2].set_addr(
                PhysAddr::new((&*tables.p1 as *const PageTable) as u64),
                user_flags(),
            );
            tables.p1[0].set_addr(PhysAddr::new(0x1000), user_flags());
            tables.p1[1].set_addr(PhysAddr::new(0x2000), user_flags());
            tables
        }

        fn entry(&mut self, level: usize) -> &mut PageTableEntry {
            match level {
                0 => &mut self.p4[0],
                1 => &mut self.p3[0],
                2 => &mut self.p2[2],
                3 => &mut self.p1[0],
                _ => unreachable!(),
            }
        }

        fn validate(&mut self, range: UserRange) -> Result<(), UserPageValidationError> {
            // Every child frame in p4 belongs to this fixture and stays
            // allocated until the mapper's borrow ends.
            let mapper = unsafe { OffsetPageTable::new(&mut self.p4, VirtAddr::zero()) };
            validate_user_range(&mapper, range)
        }
    }

    fn range(access: UserAccess) -> UserRange {
        UserRange::new(START, 1, access, 1).unwrap()
    }

    #[test]
    fn validates_cross_page_range_and_rejects_missing_tail() {
        let mut tables = Tables::new();
        let range = UserRange::new(START + 4095, 2, UserAccess::Read, 2).unwrap();
        assert_eq!(tables.validate(range), Ok(()));
        tables.p1[1].set_unused();
        assert_eq!(
            tables.validate(range),
            Err(UserPageValidationError::Unmapped { page: START + 4096 })
        );
    }

    #[test]
    fn every_level_restricts_user_access_and_writes() {
        for level in 0..4 {
            let mut tables = Tables::new();
            tables
                .entry(level)
                .set_flags(user_flags() - PageTableFlags::USER_ACCESSIBLE);
            assert_eq!(
                tables.validate(range(UserAccess::Read)),
                Err(UserPageValidationError::NotUserAccessible { page: START })
            );
            tables
                .entry(level)
                .set_flags(user_flags() - PageTableFlags::WRITABLE);
            assert_eq!(tables.validate(range(UserAccess::Read)), Ok(()));
            assert_eq!(
                tables.validate(range(UserAccess::Write)),
                Err(UserPageValidationError::NotWritable { page: START })
            );
        }
    }

    #[test]
    fn large_page_alignment_accepts_pat_and_rejects_reserved_bits() {
        for level in [1, 2] {
            let mut tables = Tables::new();
            let flags = user_flags() | PageTableFlags::HUGE_PAGE;
            tables.entry(level).set_addr(PhysAddr::zero(), flags);
            assert_eq!(tables.validate(range(UserAccess::Write)), Ok(()));
            // PAT is allowed in bit 12 for both 1 GiB and 2 MiB leaves.
            tables.entry(level).set_addr(PhysAddr::new(1 << 12), flags);
            assert_eq!(tables.validate(range(UserAccess::Write)), Ok(()));
            tables.entry(level).set_addr(PhysAddr::new(1 << 13), flags);
            assert_eq!(
                tables.validate(range(UserAccess::Read)),
                Err(UserPageValidationError::Unmapped { page: START })
            );
        }
    }

    #[test]
    fn large_pages_inherit_parent_permissions() {
        for leaf in [1, 2] {
            for parent in 0..leaf {
                let mut tables = Tables::new();
                tables
                    .entry(leaf)
                    .set_addr(PhysAddr::zero(), user_flags() | PageTableFlags::HUGE_PAGE);
                tables
                    .entry(parent)
                    .set_flags(user_flags() - PageTableFlags::USER_ACCESSIBLE);
                assert_eq!(
                    tables.validate(range(UserAccess::Read)),
                    Err(UserPageValidationError::NotUserAccessible { page: START })
                );
                tables
                    .entry(parent)
                    .set_flags(user_flags() - PageTableFlags::WRITABLE);
                assert_eq!(
                    tables.validate(range(UserAccess::Write)),
                    Err(UserPageValidationError::NotWritable { page: START })
                );
            }
        }
    }

    #[test]
    fn rejects_absent_entries_and_invalid_pml4_huge_entry() {
        for level in 0..4 {
            let mut tables = Tables::new();
            tables
                .entry(level)
                .set_flags(user_flags() - PageTableFlags::PRESENT);
            assert_eq!(
                tables.validate(range(UserAccess::Read)),
                Err(UserPageValidationError::Unmapped { page: START })
            );
        }
        let mut tables = Tables::new();
        tables.p4[0].set_flags(user_flags() | PageTableFlags::HUGE_PAGE);
        assert_eq!(
            tables.validate(range(UserAccess::Read)),
            Err(UserPageValidationError::Unmapped { page: START })
        );
    }

    #[test]
    fn empty_range_does_not_need_a_mapping() {
        let mut tables = Tables::new();
        tables.p4[0].set_unused();
        assert_eq!(
            tables.validate(UserRange::new(0, 0, UserAccess::Write, 0).unwrap()),
            Ok(())
        );
    }
}
