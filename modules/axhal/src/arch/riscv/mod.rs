#[macro_use]
mod macros;

mod context;
mod trap;

use core::cell::OnceCell;
use memory_addr::{PhysAddr, VirtAddr};
use riscv::asm;
use riscv::register::{satp, sstatus, stvec};
use crate::paging::PageTable;
use crate::mem::PAGE_SIZE_4K;

pub use self::context::{GeneralRegisters, TaskContext, TrapFrame, TRAPFRAME_SIZE, STACK_ALIGN};

pub const TASK_SIZE: usize = 0x40_0000_0000;
pub const STACK_SIZE: usize = 32 * PAGE_SIZE_4K;

/*
 * This is the location that an ET_DYN program is loaded if exec'ed.
 * Typical use of this is to invoke "./ld.so someprog" to test out
 * a new version of the loader.
 * We need to make sure that it is out of the way of the program
 * that it will "exec", and that there is sufficient room for the brk.
 */
pub const ELF_ET_DYN_BASE: usize = (TASK_SIZE / 3) * 2;

/*
 * This decides where the kernel will search for a free chunk of vm
 * space during mmap's.
 */
pub const TASK_UNMAPPED_BASE: usize = (TASK_SIZE / 3) & !(PAGE_SIZE_4K - 1);

/// Status register flags
pub const SR_SPIE:      usize = 0x00000020;  /* Previous Supervisor IE */
pub const SR_FS_INITIAL:usize = 0x00002000;
pub const SR_UXL_64:    usize = 0x200000000; /* XLEN = 64 for U-mode */

static mut KERNEL_PAGE_TABLE: OnceCell<PageTable> = OnceCell::new();

#[inline]
pub fn enable_sum() {
    unsafe { sstatus::set_sum() }
}

#[inline]
pub fn disable_sum() {
    unsafe { sstatus::clear_sum() }
}

/// Allows the current CPU to respond to interrupts.
#[inline]
pub fn enable_irqs() {
    unsafe { sstatus::set_sie() }
}

/// Makes the current CPU to ignore interrupts.
#[inline]
pub fn disable_irqs() {
    unsafe { sstatus::clear_sie() }
}

/// Returns whether the current CPU is allowed to respond to interrupts.
#[inline]
pub fn irqs_enabled() -> bool {
    sstatus::read().sie()
}

/// Relaxes the current CPU and waits for interrupts.
///
/// It must be called with interrupts enabled, otherwise it will never return.
#[inline]
pub fn wait_for_irqs() {
    unsafe { riscv::asm::wfi() }
}

/// Halt the current CPU.
#[inline]
pub fn halt() {
    disable_irqs();
    unsafe { riscv::asm::wfi() } // should never return
}

/// Reads the register that stores the current page table root.
///
/// Returns the physical address of the page table root.
#[inline]
pub fn read_page_table_root() -> PhysAddr {
    PhysAddr::from(satp::read().ppn() << 12)
}

/// Writes the register to update the current page table root.
///
/// # Safety
///
/// This function is unsafe as it changes the virtual memory address space.
pub unsafe fn write_page_table_root(root_paddr: PhysAddr) {
    let old_root = read_page_table_root();
    trace!("set page table root: {:#x} => {:#x}", old_root, root_paddr);
    if old_root != root_paddr {
        satp::set(satp::Mode::Sv39, 0, root_paddr.as_usize() >> 12);
        asm::sfence_vma_all();
    }
}

/// Flushes the TLB.
///
/// If `vaddr` is [`None`], flushes the entire TLB. Otherwise, flushes the TLB
/// entry that maps the given virtual address.
#[inline]
pub fn flush_tlb(vaddr: Option<VirtAddr>) {
    unsafe {
        if let Some(vaddr) = vaddr {
            asm::sfence_vma(0, vaddr.as_usize())
        } else {
            asm::sfence_vma_all();
        }
    }
}

/// Writes Supervisor Trap Vector Base Address Register (`stvec`).
#[inline]
pub fn set_trap_vector_base(stvec: usize) {
    unsafe { stvec::write(stvec, stvec::TrapMode::Direct) }
}

/// Reads the thread pointer of the current CPU.
///
/// It is used to implement TLS (Thread Local Storage).
#[inline]
pub fn read_thread_pointer() -> usize {
    let tp;
    unsafe { core::arch::asm!("mv {}, tp", out(reg) tp) };
    tp
}

/// Writes the thread pointer of the current CPU.
///
/// It is used to implement TLS (Thread Local Storage).
///
/// # Safety
///
/// This function is unsafe as it changes the CPU states.
#[inline]
pub unsafe fn write_thread_pointer(tp: usize) {
    core::arch::asm!("mv tp, {}", in(reg) tp)
}

pub fn setup_page_table_root(pt: PageTable) {
    unsafe {
        let _ = KERNEL_PAGE_TABLE.set(pt);
        write_page_table_root(KERNEL_PAGE_TABLE.get().unwrap().root_paddr());
    }
}

pub fn reuse_page_table_root() {
    unsafe {
        assert!(KERNEL_PAGE_TABLE.get().is_some());
        write_page_table_root(KERNEL_PAGE_TABLE.get().unwrap().root_paddr());
    }
}

pub fn dup_kernel_pg_dir() -> PageTable {
    unsafe { KERNEL_PAGE_TABLE.get().unwrap().clone() }
}
