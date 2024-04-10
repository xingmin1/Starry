#![no_std]

#[macro_use]
extern crate log;
extern crate alloc;
use alloc::sync::Arc;

use core::ops::Deref;
use core::mem::ManuallyDrop;
use core::{alloc::Layout, cell::UnsafeCell, ptr::NonNull};
use core::sync::atomic::AtomicUsize;
use axhal::arch::TaskContext as ThreadStruct;
use axhal::mem::VirtAddr;
use axhal::trap::{TRAPFRAME_SIZE, STACK_ALIGN};
use memory_addr::{align_up_4k, align_down, PAGE_SIZE_4K};
use spinlock::SpinNoIrq;
use axhal::arch::write_page_table_root0;
use axhal::paging::PageTable;

pub const THREAD_SIZE: usize = 32 * PAGE_SIZE_4K;

pub type Pid = usize;

pub struct TaskStack {
    ptr: NonNull<u8>,
    layout: Layout,
}

impl TaskStack {
    pub fn alloc(size: usize) -> Self {
        let layout = Layout::from_size_align(size, 16).unwrap();
        Self {
            ptr: NonNull::new(unsafe { alloc::alloc::alloc(layout) }).unwrap(),
            layout,
        }
    }

    pub const fn top(&self) -> usize {
        unsafe { core::mem::transmute(self.ptr.as_ptr().add(self.layout.size())) }
    }
}

impl Drop for TaskStack {
    fn drop(&mut self) {
        unsafe { alloc::alloc::dealloc(self.ptr.as_ptr(), self.layout) }
    }
}

pub struct SchedInfo {
    pid:    Pid,
    tgid:   Pid,

    pgd: Option<Arc<SpinNoIrq<PageTable>>>,
    pub mm_id: AtomicUsize,
    pub active_mm_id: AtomicUsize,

    pub entry: Option<*mut dyn FnOnce()>,
    pub kstack: Option<TaskStack>,

    /* CPU-specific state of this task: */
    pub thread: UnsafeCell<ThreadStruct>,
}

unsafe impl Send for SchedInfo {}
unsafe impl Sync for SchedInfo {}

impl SchedInfo {
    pub fn new(pid: Pid) -> Self {
        Self {
            pid,
            tgid: pid,

            pgd: None,
            mm_id: AtomicUsize::new(0),
            active_mm_id: AtomicUsize::new(0),

            entry: None,
            kstack: None,

            thread: UnsafeCell::new(ThreadStruct::new()),
        }
    }

    pub fn pid(&self) -> Pid {
        self.pid
    }

    pub fn tgid(&self) -> usize {
        self.tgid
    }

    pub fn try_pgd(&self) -> Option<Arc<SpinNoIrq<PageTable>>> {
        self.pgd.as_ref().and_then(|pgd| Some(pgd.clone()))
    }

    pub fn dup_sched_info(&self, pid: Pid) -> Arc<Self> {
        info!("dup_sched_info...");
        let mut info = SchedInfo::new(pid);
        info.kstack = Some(TaskStack::alloc(align_up_4k(THREAD_SIZE)));
        info.pgd = self.pgd.clone();
        info.mm_id = AtomicUsize::new(0);
        info.active_mm_id = AtomicUsize::new(0);
        Arc::new(info)
    }

    pub fn pt_regs(&self) -> usize {
        self.kstack.as_ref().unwrap().top() - align_down(TRAPFRAME_SIZE, STACK_ALIGN)
    }

    #[inline]
    pub const unsafe fn ctx_mut_ptr(&self) -> *mut ThreadStruct {
        self.thread.get()
    }

    pub fn reset(&mut self, entry: Option<*mut dyn FnOnce()>, entry_func: usize, tls: VirtAddr) {
        self.entry = entry;
        self.kstack = Some(TaskStack::alloc(align_up_4k(THREAD_SIZE)));
        let sp = self.pt_regs();
        self.thread.get_mut().init(entry_func, sp.into(), tls);
    }
}

/// The reference type of a task.
pub type CtxRef = Arc<SchedInfo>;

/// A wrapper of [`TaskCtxRef`] as the current task contex.
pub struct CurrentCtx(ManuallyDrop<CtxRef>);

impl CurrentCtx {
    pub(crate) fn try_get() -> Option<Self> {
        let ptr: *const SchedInfo = axhal::cpu::current_task_ptr();
        if !ptr.is_null() {
            Some(Self(unsafe { ManuallyDrop::new(CtxRef::from_raw(ptr)) }))
        } else {
            None
        }
    }

    pub(crate) fn get() -> Self {
        Self::try_get().expect("current sched info is uninitialized")
    }

    pub fn ptr_eq(&self, other: &CtxRef) -> bool {
        Arc::ptr_eq(&self, other)
    }

    /// Converts [`CurrentTask`] to [`TaskRef`].
    pub fn as_task_ref(&self) -> &CtxRef {
        &self.0
    }

    pub unsafe fn set_current(prev: Self, next: CtxRef) {
        info!("CurrentCtx::set_current...");
        let Self(arc) = prev;
        ManuallyDrop::into_inner(arc); // `call Arc::drop()` to decrease prev task reference count.
        let ptr = Arc::into_raw(next.clone());
        axhal::cpu::set_current_task_ptr(ptr);
    }
}

impl Deref for CurrentCtx {
    type Target = CtxRef;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub fn current_ctx() -> CurrentCtx {
    CurrentCtx::get()
}

pub fn try_current_ctx() -> Option<CurrentCtx> {
    CurrentCtx::try_get()
}

pub fn switch_mm(prev_mm_id: usize, next_mm_id: usize, next_pgd: Arc<SpinNoIrq<PageTable>>) {
    if prev_mm_id == next_mm_id {
        return;
    }
    error!("###### switch prev {} next {}; paddr {:#X}",
        prev_mm_id, next_mm_id, next_pgd.lock().root_paddr());
    unsafe {
        write_page_table_root0(next_pgd.lock().root_paddr().into());
    }
}
