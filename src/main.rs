fn threaded_add(nthread: u16, niter: u16) -> u32 {
    let lock = mcslock::Lock::new(0);
    let _ = std::thread::scope(|scope| {
        for _ in 0..nthread {
            let mut node = lock.node();
            scope.spawn(move || {
                for _ in 0..niter {
                    let mut v = node.lock();
                    *v += 1;
                    std::thread::yield_now();
                }
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
    let v = threaded_add(16, 128);
    println!("v = {v}");
}
