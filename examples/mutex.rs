#![feature(allocator_api)]
use core::{
    cell::{Cell, UnsafeCell},
    ops::{Deref, DerefMut},
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
};
use std::{
    sync::Arc,
    thread::{self, park, sleep, Builder, Thread},
    time::Duration,
};

use pinned_init::*;
#[allow(unused_attributes)]
#[path = "./linked_list.rs"]
pub mod linked_list;
use linked_list::*;

pub struct SpinLock {
    inner: AtomicBool,
}

impl SpinLock {
    #[inline]
    pub fn acquire(&self) -> SpinLockGuard<'_> {
        while self
            .inner
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {}
        SpinLockGuard(self)
    }

    #[inline]
    pub const fn new() -> Self {
        Self {
            inner: AtomicBool::new(false),
        }
    }
}

pub struct SpinLockGuard<'a>(&'a SpinLock);

impl Drop for SpinLockGuard<'_> {
    #[inline]
    fn drop(&mut self) {
        self.0.inner.store(false, Ordering::Release);
    }
}

#[pin_data]
pub struct CMutex<T> {
    #[pin]
    wait_list: ListHead,
    spin_lock: SpinLock,
    locked: Cell<bool>,
    data: UnsafeCell<T>,
}

impl<T> CMutex<T> {
    #[inline]
    pub fn new(val: T) -> impl PinInit<Self> {
        pin_init!(Self {
            wait_list <- ListHead::new(),
            spin_lock: SpinLock::new(),
            locked: Cell::new(false),
            data: UnsafeCell::new(val),
        })
    }

    #[inline]
    pub fn lock(&self) -> CMutexGuard<'_, T> {
        let mut sguard = self.spin_lock.acquire();
        if self.locked.get() {
            stack_pin_init!(let wait_entry = WaitEntry::insert_new(&self.wait_list));
            let wait_entry = match wait_entry {
                Ok(w) => w,
                Err(e) => match e {},
            };
            // println!("wait list length: {}", self.wait_list.size());
            while self.locked.get() {
                drop(sguard);
                park();
                sguard = self.spin_lock.acquire();
            }
            drop(wait_entry);
        }
        self.locked.set(true);
        CMutexGuard { mtx: self }
    }
}

unsafe impl<T: Send> Send for CMutex<T> {}
unsafe impl<T: Send> Sync for CMutex<T> {}

pub struct CMutexGuard<'a, T> {
    mtx: &'a CMutex<T>,
}

impl<'a, T> Drop for CMutexGuard<'a, T> {
    #[inline]
    fn drop(&mut self) {
        let sguard = self.mtx.spin_lock.acquire();
        self.mtx.locked.set(false);
        if let Some(list_field) = self.mtx.wait_list.next() {
            let wait_entry = list_field.as_ptr().cast::<WaitEntry>();
            unsafe { (*wait_entry).thread.unpark() };
        }
        drop(sguard);
    }
}

impl<'a, T> Deref for CMutexGuard<'a, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.mtx.data.get() }
    }
}

impl<'a, T> DerefMut for CMutexGuard<'a, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.mtx.data.get() }
    }
}

#[pin_data]
#[repr(C)]
struct WaitEntry {
    #[pin]
    wait_list: ListHead,
    thread: Thread,
}

impl WaitEntry {
    #[inline]
    fn insert_new(list: &ListHead) -> impl PinInit<Self> + '_ {
        pin_init!(Self {
            thread: thread::current(),
            wait_list <- ListHead::insert_prev(list),
        })
    }
}

fn main() {
    let mtx: Pin<Arc<CMutex<usize>>> = Arc::pin_init(CMutex::new(0)).unwrap();
    let mut handles = vec![];
    let thread_count = 20;
    let workload = 1_000_000;
    for i in 0..thread_count {
        let mtx = mtx.clone();
        handles.push(
            Builder::new()
                .name(format!("worker #{i}"))
                .spawn(move || {
                    for _ in 0..workload {
                        *mtx.lock() += 1;
                    }
                    println!("{i} halfway");
                    sleep(Duration::from_millis((i as u64) * 10));
                    for _ in 0..workload {
                        *mtx.lock() += 1;
                    }
                    println!("{i} finished");
                })
                .expect("should not fail"),
        );
    }
    for h in handles {
        h.join().expect("thread paniced");
    }
    println!("{:?}", &*mtx.lock());
    assert_eq!(*mtx.lock(), workload * thread_count * 2);
}
