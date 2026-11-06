use std::collections::VecDeque;
use std::sync::Arc;

use eagler_world::ChunkPos;
use parking_lot::Mutex;

/// A coalescible chunk-mesh rebuild request sent from world updates to workers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MeshJob {
    pub chunk: ChunkPos,
    pub section_y: i32,
    pub world_revision: u64,
}

/// Bounded, non-blocking hand-off queue shared by the render coordinator and
/// mesh workers. Uses parking_lot's compact mutex primitive, not std::sync::Mutex.
#[derive(Clone, Debug)]
pub struct MeshJobQueue {
    inner: Arc<Mutex<VecDeque<MeshJob>>>,
    capacity: usize,
}

impl MeshJobQueue {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(capacity))),
            capacity,
        }
    }

    /// Enqueue a rebuild unless the queue is full. A rejected job can be
    /// retried after the consumer drains older work.
    pub fn push(&self, job: MeshJob) -> Result<(), MeshJob> {
        let mut queue = self.inner.lock();
        if queue.len() >= self.capacity {
            return Err(job);
        }
        queue.push_back(job);
        Ok(())
    }

    pub fn pop(&self) -> Option<MeshJob> {
        self.inner.lock().pop_front()
    }

    pub fn drain(&self, maximum: usize) -> Vec<MeshJob> {
        let mut queue = self.inner.lock();
        let count = maximum.min(queue.len());
        queue.drain(..count).collect()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use eagler_world::ChunkPos;

    use super::{MeshJob, MeshJobQueue};

    #[test]
    fn queue_is_fifo_and_rejects_overflow_without_losing_work() {
        let queue = MeshJobQueue::new(2);
        let first = MeshJob { chunk: ChunkPos::new(1, 2), section_y: 0, world_revision: 1 };
        let second = MeshJob { chunk: ChunkPos::new(3, 4), section_y: 1, world_revision: 2 };
        let third = MeshJob { chunk: ChunkPos::new(5, 6), section_y: 2, world_revision: 3 };

        assert_eq!(queue.push(first), Ok(()));
        assert_eq!(queue.push(second), Ok(()));
        assert_eq!(queue.push(third), Err(third));
        assert_eq!(queue.pop(), Some(first));
        assert_eq!(queue.pop(), Some(second));
        assert!(queue.is_empty());
    }

    #[test]
    fn queue_can_be_shared_between_worker_threads() {
        let queue = MeshJobQueue::new(128);
        let producer = queue.clone();
        let worker = thread::spawn(move || {
            for revision in 0..64 {
                producer.push(MeshJob {
                    chunk: ChunkPos::new(revision as i32, -1),
                    section_y: 0,
                    world_revision: revision,
                }).unwrap();
            }
        });
        worker.join().unwrap();
        assert_eq!(queue.len(), 64);
        assert_eq!(queue.drain(64).len(), 64);
        assert!(queue.is_empty());
    }
}
