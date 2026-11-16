//! [`par_map`]: a parallel map on scoped threads, for the hashes and the decodes of the trees.

use std::{
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

/// Runs `f` for each index in `0..n` on up to `threads` scoped threads, and returns the results
/// in index order. Each thread takes the next block of indexes when it is free.
///
/// The threads run at the lowest priority: they hash on the cores that the block pipeline and
/// the gRPC server leave idle, and give way to them. They hold no lock and no LMDB transaction,
/// so a low priority cannot hold up another thread. With one thread, `f` runs on the caller, at
/// its priority.
pub(crate) fn par_map<T: Send>(n: usize, threads: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    if threads <= 1 || n <= 1 {
        return (0..n).map(f).collect();
    }
    let block = n.div_ceil(threads * 8).max(1);
    let blocks = n.div_ceil(block);
    let next = AtomicUsize::new(0);
    let mut parts: Vec<(usize, Vec<T>)> = thread::scope(|scope| {
        let workers: Vec<_> = (0..threads.min(blocks))
            .map(|_| {
                scope.spawn(|| {
                    lower_priority();
                    let mut done = Vec::new();
                    loop {
                        let taken = next.fetch_add(1, Ordering::Relaxed);
                        if taken >= blocks {
                            return done;
                        }
                        let indexes = taken * block..((taken + 1) * block).min(n);
                        done.push((taken, indexes.map(&f).collect()));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().expect("a tree worker does not panic"))
            .collect()
    });
    parts.sort_unstable_by_key(|(taken, _)| *taken);
    parts.into_iter().flat_map(|(_, part)| part).collect()
}

/// Gives the calling thread the lowest priority, nice 19. Linux keeps a nice value for each
/// thread, so a scoped worker changes only itself, and the change ends with the thread. If the call
/// fails, the worker keeps its priority.
#[cfg(target_os = "linux")]
fn lower_priority() {
    // SAFETY: `setpriority` reads and writes no memory of the process. On Linux, `PRIO_PROCESS`
    // with `who = 0` is the calling thread.
    let _ = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 19) };
}

/// Other systems set the priority of the whole process. A worker keeps its priority there.
#[cfg(not(target_os = "linux"))]
const fn lower_priority() {}
