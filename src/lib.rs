//! MCS locks in Rust.

#![no_std]

use core::cell::UnsafeCell;
use core::marker::{Send, Sync};
use core::ops::{Deref, DerefMut};
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

pub struct Lock<T: ?Sized> {
    held: AtomicBool,
    head: AtomicPtr<Node>,
    tail: AtomicPtr<Node>,
    data: UnsafeCell<T>,
}

struct Node {
    next: AtomicPtr<Node>,
    waiting: AtomicBool,
}

impl<T> Lock<T> {
    pub const fn new(value: T) -> Lock<T> {
        Lock {
            held: AtomicBool::new(false),
            head: AtomicPtr::new(ptr::null_mut()),
            tail: AtomicPtr::new(ptr::null_mut()),
            data: UnsafeCell::new(value),
        }
    }

    pub fn into_inner(self) -> T {
        assert!(self.tail.load(Ordering::Acquire).is_null());
        self.data.into_inner()
    }
}

impl<T: ?Sized> Lock<T> {
    /// Acquires the lock and enters a critical section guarded
    /// by it.  Returns a LockGuard that will release the lock
    /// when it drops.
    pub fn lock(&self) -> LockGuard<'_, T> {
        let node = Node {
            next: AtomicPtr::new(ptr::null_mut()),
            waiting: AtomicBool::new(false),
        };
        let nodeptr = (&raw const node).cast_mut();
        let predptr = self.tail.swap(nodeptr, Ordering::AcqRel);
        if let Some(predecessor) = unsafe { predptr.as_ref() } {
            node.waiting.store(true, Ordering::Release);
            predecessor.next.store(nodeptr, Ordering::Release);
            while node.waiting.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
        }
        // With ordinary MCS locks, it is not possible for a
        // thread to observe an empty queue if the lock is
        // held, because the lock holder atomically swapped
        // its `Node` onto `tail` at some previous point, and
        // does not remove it from the queue until `.unlock()`.
        //
        // However, in our implementation, we need to be robust
        // against the lockguard leaking without its `.drop()`
        // method being called, *and* the `Node` being destroyed.
        //
        // To guard against the second, the node is local to
        // `.lock()`; it cannot be destroyed while we're waiting
        // on the queue.  But it also means it does not survive
        // beyond `.lock()`, so we cannot use it in `.unlock()`
        // to find the next waiter and transfer the lock to it.
        // Thus, we must remove it _after_ entering the critical
        // section, but _before_ we leave `.lock()`, and so a
        // subsequent thread _could_ observe an empty queue
        // while we are in the critical section if it calls
        // `.lock()` after we have removed ourselves from the
        // queue and there were no other pending waiters.
        //
        // To guard against this, we use a separate value to
        // track that the lock is held; this is basically a
        // spinlock, which appears to defeat the purpose of
        // using an MCS lock in the first place, but observe
        // that only the waiter at the head of the queue will
        // ever spin on it.
        while self.held.load(Ordering::Relaxed) || self.held.swap(true, Ordering::AcqRel) {
            core::hint::spin_loop();
        }
        // If we have gotten here, then we must be the lock
        // holder, and we are at the front of the queue; no
        // queue node can point to us.
        //
        // If our next pointer is nil, then at the moment we
        // loaded it, no one had linked _from_ us yet, and we
        // may have been the node at the tail of the queue.
        // This may have changed, but at least there is some
        // chance for success if we try to unlink ourselves.
        //
        // On the other hand, if next was not nil, then we point
        // to a waiter and we are not the tail node.  Unlinking
        // with a CAS can never succeed, so we don't bother
        // trying.
        let mut nextptr = node.next.load(Ordering::Acquire);
        if nextptr.is_null()
            && self
                .tail
                .compare_exchange(
                    nodeptr,
                    ptr::null_mut(),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
        {
            // If removing ourselves from the tail of the queue
            // failed, then we were racing against another
            // thread adding its node at the end, and we lost.
            // That thread now has a reference to our node and
            // is modifying our next pointer to point to its
            // node.  Wait for it to finish.
            while nextptr.is_null() {
                nextptr = node.next.load(Ordering::Acquire);
            }
        }
        // If `nextptr` was nil and we successfully unlinked
        // ourselves from the tail of the queue, then no one
        // else can modify our `next` pointer to link from us.
        // Thus, there are either no pending waiters at all,
        // or we completely avoided a race with a later arrival.
        // In the latter case, that waiter is pending, blocked
        // spinning on `self.held`.
        //
        // Conversely, if our `next` was not nil, or was nil
        // but we lost the race removing ourselves from the
        // queue, then we have a pointer to the waiter that
        // is now at the head of the queue.  Store it in
        // `self.head` so that `.unlock()` can find it to
        // transfer the lock to it.
        if !nextptr.is_null() {
            self.head.store(nextptr, Ordering::Release);
        }
        // We have now completed entry into the critical section
        // and set up state so that we can unblock the next
        // waiter when we unlock.  We will not use `node` again,
        // and it is safe to let it drop.
        LockGuard {
            data: unsafe { &mut *self.data.get() },
            lock: self,
        }
    }

    /// Releases the lock, assuming it is held.  Private, so
    /// that the only way to unlock is through the `drop` method
    /// of a `LockGuard<T>` (that itself can only exist as the
    /// result of a successful call to `lock`), or by calling
    /// the unsafe `force_unlock` method.
    fn unlock(&self) {
        let headptr = self.head.load(Ordering::Acquire);
        self.head.store(ptr::null_mut(), Ordering::Release);
        self.held.store(false, Ordering::Release);
        if let Some(head) = unsafe { headptr.as_ref() } {
            head.waiting.store(false, Ordering::Release);
        }
    }

    /// Force-releases the lock.
    ///
    /// Suppose that a lock guard holding the lock is leaked
    /// (e.g., via `std::mem::forget()`); its `drop` method
    /// can never be called to unlock.  This function provides
    /// an unsafe escape hatch to release the lock.
    ///
    /// # Safety
    ///
    /// Callers must ensure that the lock is currently locked,
    /// that the underlying object is in a state that it is
    /// safe to unlock, and that nothing else will try to unlock
    /// the current lock-holder later; leaked LockGuards must
    /// be forgotten: their `drop` methods must never be
    /// invoked, and their borrows to the lock-owned data must
    /// be gone.
    pub unsafe fn force_unlock(&self) {
        self.unlock();
    }
}

unsafe impl<T: ?Sized + Send> Send for Lock<T> {}
unsafe impl<T: ?Sized + Send> Sync for Lock<T> {}

pub struct LockGuard<'a, T: ?Sized> {
    lock: &'a Lock<T>,
    data: &'a mut T,
}

impl<T: ?Sized> Deref for LockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.data
    }
}

impl<T: ?Sized> DerefMut for LockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.data
    }
}

impl<T: ?Sized> Drop for LockGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.unlock();
    }
}
