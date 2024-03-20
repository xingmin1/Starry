#![cfg_attr(not(test), no_std)]

extern crate alloc;
use alloc::string::String;

use axhal::trap::SyscallHandler;
use axhal::arch::TrapFrame;
//use axhal::mem::virt_to_phys;
use core::arch::asm;
use memory_addr::{align_up_4k, is_aligned_4k};
use mmap::{MAP_FIXED, MAP_ANONYMOUS};
use axfile::fops::File;
use axfile::fops::OpenOptions;
use alloc::sync::Arc;
use spinlock::SpinNoIrq;
use axerrno::LinuxError;

#[macro_use]
extern crate log;

struct LinuxSyscallHandler;

#[crate_interface::impl_interface]
impl SyscallHandler for LinuxSyscallHandler {
    fn handle_syscall(tf: &mut TrapFrame) {
        let eid = tf.regs.a7;
        error!("Syscall: {:#x}", eid);
        tf.regs.a0 = match eid {
            LINUX_SYSCALL_OPENAT => {
                linux_syscall_openat(tf)
            },
            LINUX_SYSCALL_CLOSE => {
                linux_syscall_close(tf)
            },
            LINUX_SYSCALL_READ => {
                linux_syscall_read(tf)
            },
            LINUX_SYSCALL_WRITE => {
                linux_syscall_write(tf)
            },
            LINUX_SYSCALL_WRITEV => {
                linux_syscall_writev(tf)
            },
            LINUX_SYSCALL_READLINKAT => {
                usize::MAX
            },
            LINUX_SYSCALL_FSTATAT => {
                0
            },
            LINUX_SYSCALL_UNAME => {
                linux_syscall_uname(tf)
            },
            LINUX_SYSCALL_BRK => {
                linux_syscall_brk(tf)
            },
            LINUX_SYSCALL_MUNMAP => {
                linux_syscall_munmap(tf)
            },
            LINUX_SYSCALL_MMAP => {
                linux_syscall_mmap(tf)
            },
            LINUX_SYSCALL_EXIT => {
                linux_syscall_exit(tf)
            },
            LINUX_SYSCALL_EXIT_GROUP => {
                linux_syscall_exit_group(tf)
            },
            _ => {
                0
            }
        };
        tf.sepc += 4;
    }
}

//
// Linux syscall
//
const LINUX_SYSCALL_OPENAT:     usize = 0x38;
const LINUX_SYSCALL_CLOSE:      usize = 0x39;
const LINUX_SYSCALL_READ:       usize = 0x3f;
const LINUX_SYSCALL_WRITE:      usize = 0x40;
const LINUX_SYSCALL_WRITEV:     usize = 0x42;
const LINUX_SYSCALL_READLINKAT: usize = 0x4e;
const LINUX_SYSCALL_FSTATAT:    usize = 0x4f;
const LINUX_SYSCALL_EXIT:       usize = 0x5d;
const LINUX_SYSCALL_EXIT_GROUP: usize = 0x53;
const LINUX_SYSCALL_UNAME:      usize = 0xa0;
const LINUX_SYSCALL_BRK:        usize = 0xd6;
const LINUX_SYSCALL_MUNMAP:     usize = 0xd7;
const LINUX_SYSCALL_MMAP:       usize = 0xde;


#[derive(Debug)]
#[repr(C)]
struct iovec {
    iov_base: usize,
    iov_len: usize,
}

/// # Safety
///
/// The caller must ensure that the pointer is valid and
/// points to a valid C string.
/// The string must be null-terminated.
pub unsafe fn get_str_len(ptr: *const u8) -> usize {
    let mut cur = ptr as usize;
    while *(cur as *const u8) != 0 {
        cur += 1;
    }
    cur - ptr as usize
}

/// # Safety
///
/// The caller must ensure that the pointer is valid and
/// points to a valid C string.
pub fn raw_ptr_to_ref_str(ptr: *const u8) -> &'static str {
    let len = unsafe { get_str_len(ptr) };
    let slice = unsafe { core::slice::from_raw_parts(ptr, len) };
    if let Ok(s) = core::str::from_utf8(slice) {
        s
    } else {
        panic!("not utf8 slice");
    }
}

pub fn get_user_str(ptr: usize) -> String {
    let ptr = ptr as *const u8;
    axhal::arch::enable_sum();
    let ptr = raw_ptr_to_ref_str(ptr);
    let s = String::from(ptr);
    axhal::arch::disable_sum();
    s
}

fn linux_syscall_openat(tf: &TrapFrame) -> usize {
    let dtd = tf.regs.a0;
    let filename = tf.regs.a1;
    let flags = tf.regs.a2;
    let mode = tf.regs.a3;

    let filename = get_user_str(filename);
    error!("filename: {}\n", filename);
    //////////////////////////
    let mut opts = OpenOptions::new();
    opts.read(true);

    let current = task::current();
    let fs = current.fs.lock();
    let file = match File::open(&filename, &opts, &fs) {
        Ok(f) => f,
        Err(e) => {
            return (-LinuxError::from(e).code()) as usize;
        },
    };
    let fd = current.filetable.lock().insert(Arc::new(SpinNoIrq::new(file)));
    //////////////////////////
    error!("linux_syscall_openat fd {}", fd);
    fd
}

fn linux_syscall_close(tf: &TrapFrame) -> usize {
    error!("linux_syscall_close");
    0
}

fn linux_syscall_read(tf: &TrapFrame) -> usize {
    let fd = tf.regs.a0;
    let buf = tf.regs.a1;
    let count = tf.regs.a2;

    let user_buf = unsafe {
        core::slice::from_raw_parts_mut(buf as *mut u8, count)
    };

    let current = task::current();
    let filetable = current.filetable.lock();
    let file = filetable.get_file(fd).unwrap();
    let mut pos = 0;
    assert!(count < 1024);
    let mut buf: [u8; 1024] = [0; 1024];
    while pos < count {
        let ret = file.lock().read(&mut buf[pos..]).unwrap();
        if ret == 0 {
            break;
        }
        pos += ret;
    }
    axhal::arch::enable_sum();
    user_buf.copy_from_slice(&buf[..count]);
    axhal::arch::disable_sum();
    //error!("linux_syscall_read: fd {}, buf {:#X}, count {}, ret {}", fd, buf, count, pos);
    pos
}

fn linux_syscall_write(tf: &TrapFrame) -> usize {
    use core::slice;
    debug!("write: {:#x}, {:#x}, {:#x}",
        tf.regs.a0, tf.regs.a1, tf.regs.a2);

    let buf = tf.regs.a1 as *const u8;
    let size = tf.regs.a2;
    let bytes = unsafe { slice::from_raw_parts(buf as *const _, size) };
    /*
    let s = String::from_utf8(bytes.into());
    debug!("{}", s.unwrap());
    */

    axhal::arch::enable_sum();
    axhal::console::write_bytes(bytes);
    axhal::arch::disable_sum();

    return size;
}

fn linux_syscall_writev(tf: &TrapFrame) -> usize {
    use core::slice;

    debug!("writev: {:#x}, {:#x}, {:#x}",
        tf.regs.a0, tf.regs.a1, tf.regs.a2);

    let array = tf.regs.a1 as *const iovec;
    let size = tf.regs.a2;
    axhal::arch::enable_sum();
    let iov_array = unsafe { slice::from_raw_parts(array, size) };
    for iov in iov_array {
        debug!("iov: {:#X} {:#X}", iov.iov_base, iov.iov_len);
        let bytes = unsafe { slice::from_raw_parts(iov.iov_base as *const _, iov.iov_len) };
        let s = String::from_utf8(bytes.into());
        error!("{}", s.unwrap());
    }
    axhal::arch::disable_sum();

    return size;
}

// void *mmap(void *addr, size_t len, int prot, int flags, int fd, off_t off);
fn linux_syscall_mmap(tf: &TrapFrame) -> usize {
    let va = tf.regs.a0;
    let len = tf.regs.a1;
    let prot = tf.regs.a2;
    let flags = tf.regs.a3;
    let fd = tf.regs.a4;
    let offset = tf.regs.a5;
    assert!(is_aligned_4k(va));
    error!("###### mmap!!! {:#x} {:#x} {:#x} {:#x} {:#x} {:#x}", va, len, prot, flags, fd, offset);

    let current = task::current();
    let filetable = current.filetable.lock();
    let file = if (flags & MAP_ANONYMOUS) != 0 {
        None
    } else {
        filetable.get_file(fd)
    };
    mmap::mmap(va, len, prot, flags, file, offset).unwrap()
}

const UTS_LEN: usize = 64;

#[repr(C)]
struct utsname {
    sysname: [u8; UTS_LEN + 1],
    nodename: [u8; UTS_LEN + 1],
    release: [u8; UTS_LEN + 1],
    version: [u8; UTS_LEN + 1],
    machine: [u8; UTS_LEN + 1],
    domainname: [u8; UTS_LEN + 1],
}

fn linux_syscall_uname(tf: &TrapFrame) -> usize {
    let ptr = tf.regs.a0;
    info!("uname: {:#x}", ptr);

    let uname = unsafe { (ptr as *mut utsname).as_mut().unwrap() };

    init_bytes_from_str(&mut uname.sysname[..], "Linux");
    init_bytes_from_str(&mut uname.nodename[..], "host");
    init_bytes_from_str(&mut uname.domainname[..], "(none)");
    init_bytes_from_str(&mut uname.release[..], "5.9.0-rc4+");
    init_bytes_from_str(&mut uname.version[..], "#1337 SMP Fri Mar 4 09:36:42 CST 2022");
    init_bytes_from_str(&mut uname.machine[..], "riscv64");

    return 0;
}

/*
fn enable_user_access() {
    /* Enable access to user memory */
    unsafe { asm!("
        li t6, 0x00040000
        csrs sstatus, t6"
    )}
}

fn disable_user_access() {
    /* Disable access to user memory */
    unsafe { asm!("
        li t6, 0x00040000
        csrc sstatus, t6"
    )}
}
*/

fn init_bytes_from_str(dst: &mut [u8], src: &str) {
    let src = src.as_bytes();
    let (left, right) = dst.split_at_mut(src.len());
    axhal::arch::enable_sum();
    left.copy_from_slice(src);
    right.fill(0);
    axhal::arch::disable_sum();
}

/*
fn alloc_pages(
    num_pages: usize, align_pow2: usize
) -> usize {
    axalloc::global_allocator().alloc_pages(num_pages, align_pow2)
        .map(|va| virt_to_phys(va.into())).ok().unwrap().into()
}
*/

/*
fn map_region(va: usize, pa: usize, len: usize, flags: usize) {
    task::current().map_region(va, pa, len, flags);
}
*/

fn linux_syscall_brk(tf: &TrapFrame) -> usize {
    // Have a guard for mm to lock this whole function,
    // because mm.brk() and mm.set_brk() should be in a atomic context.
    let mm = task::current().mm();
    let brk = mm.lock().brk();

    let va = align_up_4k(tf.regs.a0);
    assert!(is_aligned_4k(brk));
    error!("brk!!! {:#x}, {:#x}", va, brk);

    if va == 0 {
        brk
    } else {
        assert!(va > brk);
        let offset = va - brk;
        assert!(is_aligned_4k(offset));
        //let n = offset >> PAGE_SHIFT;
        //let pa = alloc_pages(n, PAGE_SIZE_4K);
        mmap::mmap(brk, offset, 0, MAP_FIXED|MAP_ANONYMOUS, None, 0).unwrap();
        let _ = mmap::faultin_page(brk);
        //map_region(brk, pa, n*PAGE_SIZE_4K, 1);
        mm.lock().set_brk(va);
        va
    }
}

fn linux_syscall_munmap(tf: &TrapFrame) -> usize {
    let va = tf.regs.a0;
    let len = tf.regs.a1;
    debug!("munmap!!! {:#x} {:#x}", va, len);
    unimplemented!();
    //return 0;
}

fn linux_syscall_exit(tf: &TrapFrame) -> usize {
    let ret = tf.regs.a0 as i32;
    debug!("exit ...{}", ret);
    task::exit(ret);
}

fn linux_syscall_exit_group(_tf: &TrapFrame) -> usize {
    debug!("exit_group!");
    return 0;
}

pub fn init() {
}