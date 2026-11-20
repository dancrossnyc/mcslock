#[inline(never)]
fn count(lock: &mcslock::Lock<u32>, niters: u16) {
    for _ in 0..niters {
        let mut v = lock.lock();
        *v += 1;
        std::thread::yield_now();
    }
}

#[inline(never)]
fn threaded_add(nthread: u16, niter: u16) -> u32 {
    let lock = mcslock::Lock::new(0);
    std::thread::scope(|scope| {
        for _ in 0..nthread {
            scope.spawn(|| {
                count(&lock, niter);
            });
        }
    });
    lock.into_inner()
}

#[cfg(test)]
#[test]
fn threaded_add_works() {
    assert_eq!(threaded_add(16, 1024), 16384);
}

fn main() {
    let lock = mcslock::Lock::new(1);
    let mut v = lock.lock();
    *v -= 1;
    std::mem::forget(v);
    unsafe {
        lock.force_unlock();
    }
    let v = threaded_add(4, 128);
    println!("v = {v}");
}
