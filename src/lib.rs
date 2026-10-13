//! MCS locks in Rust.

use core::cell::UnsafeCell;
use core::marker::{Send, Sync};
use core::ops::{Deref, DerefMut};
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

pub struct Lock<T: ?Sized> {
    queue: AtomicPtr<Node>,
    data: UnsafeCell<T>,
}

struct Node {
    next: AtomicPtr<Node>,
    locked: AtomicBool,
}

impl<T> Lock<T> {
    pub const fn new(value: T) -> Lock<T> {
        Lock {
            queue: AtomicPtr::new(ptr::null_mut()),
            data: UnsafeCell::new(value),
        }
    }

    pub fn node(&self) -> LockNode<'_, T> {
        LockNode {
            node: Node {
                next: AtomicPtr::new(ptr::null_mut()),
                locked: AtomicBool::new(false),
            },
            lock: self,
        }
    }

    pub fn into_inner(self) -> T {
        assert!(self.queue.load(Ordering::Acquire).is_null());
        self.data.into_inner()
    }
}

unsafe impl<'a, T: Send> Send for Lock<T> {}
unsafe impl<'a, T: Send> Sync for Lock<T> {}

pub struct LockNode<'a, T> {
    node: Node,
    lock: &'a Lock<T>,
}

impl<'a, T> LockNode<'a, T> {
    pub fn lock(&mut self) -> LockGuard<'_, 'a, T> {
        let node: &Node = &self.node;
        let lock = self.lock;
        node.next.store(ptr::null_mut(), Ordering::Release);
        node.locked.store(false, Ordering::Release);
        let nodeptr = (node as *const Node).cast_mut();
        let predptr = lock.queue.swap(nodeptr, Ordering::AcqRel);
        if let Some(predecessor) = unsafe { predptr.as_ref() } {
            node.locked.store(true, Ordering::Release);
            predecessor.next.store(nodeptr, Ordering::Release);
            while node.locked.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
        }
        LockGuard {
            data: unsafe { &mut *lock.data.get() },
            node: self,
        }
    }

    fn unlock(&'a self) {
        if self.node.next.load(Ordering::Acquire).is_null() {
            let nodeptr = (&self.node as *const Node).cast_mut();
            if self
                .lock
                .queue
                .compare_exchange(
                    nodeptr,
                    ptr::null_mut(),
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                return;
            }
            while self.node.next.load(Ordering::Acquire).is_null() {
                core::hint::spin_loop();
            }
        }
        let next = unsafe { &*self.node.next.load(Ordering::Acquire) };
        next.locked.store(false, Ordering::Release);
    }
}

pub struct LockGuard<'n, 'a, T> {
    node: &'n LockNode<'a, T>,
    data: &'a mut T,
}

impl<'a, T> Deref for LockGuard<'_, 'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.data
    }
}

impl<'a, T> DerefMut for LockGuard<'_, 'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.data
    }
}

impl<'a, T> Drop for LockGuard<'_, 'a, T> {
    fn drop(&mut self) {
        self.node.unlock();
    }
}
