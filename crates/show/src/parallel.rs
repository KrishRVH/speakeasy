use std::{num::NonZero, thread};

pub(crate) fn map<T: Sync, R: Send>(items: &[T], work: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = thread::available_parallelism().map_or(1, NonZero::get);
    let per_thread = items.len().div_ceil(threads).max(1);
    let run = |chunk: &[T]| chunk.iter().map(&work).collect::<Vec<R>>();
    thread::scope(|scope| {
        // Spawn every worker before the first join; joining as they spawn would run serially.
        let workers: Vec<_> = items
            .chunks(per_thread)
            .map(|chunk| {
                thread::Builder::new()
                    .spawn_scoped(scope, move || run(chunk))
                    .map_err(|_refused| chunk)
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| match worker {
                Ok(worker) => worker
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
                Err(chunk) => run(chunk),
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn results_keep_their_order_however_the_work_is_split() {
        let items: Vec<u32> = (0..37).collect();
        assert_eq!(
            super::map(&items, |item| item.wrapping_mul(3)),
            items
                .iter()
                .map(|item| item.wrapping_mul(3))
                .collect::<Vec<_>>()
        );
        assert_eq!(super::map(&[] as &[u32], |item| *item), Vec::<u32>::new());
    }
}
