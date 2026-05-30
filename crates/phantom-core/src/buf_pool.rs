use crossbeam_queue::SegQueue;
use std::sync::Arc;

/// A lock-free pool of reusable `Vec<u8>` buffers.
///
/// Reduces allocation pressure in the hot relay path by recycling
/// buffers that have already been allocated to the right capacity.
pub struct BufferPool {
    pool: Arc<SegQueue<Vec<u8>>>,
    default_cap: usize,
}

impl BufferPool {
    pub fn new(default_cap: usize) -> Self {
        Self {
            pool: Arc::new(SegQueue::new()),
            default_cap,
        }
    }

    /// Acquire a buffer from the pool, or allocate a new one.
    pub fn acquire(&self) -> Vec<u8> {
        match self.pool.pop() {
            Some(mut buf) => {
                buf.clear();
                buf
            }
            None => Vec::with_capacity(self.default_cap),
        }
    }

    /// Return a buffer to the pool for reuse.
    /// If the buffer is oversized it is dropped to avoid unbounded growth.
    pub fn release(&self, buf: Vec<u8>) {
        if buf.capacity() <= self.default_cap * 2 {
            self.pool.push(buf);
        }
    }
}

impl Clone for BufferPool {
    fn clone(&self) -> Self {
        Self {
            pool: Arc::clone(&self.pool),
            default_cap: self.default_cap,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_reuses_buffer() {
        let pool = BufferPool::new(1024);
        let buf = pool.acquire();
        assert_eq!(buf.capacity(), 1024);
        pool.release(buf);
        let buf2 = pool.acquire();
        assert_eq!(buf2.capacity(), 1024);
    }

    #[test]
    fn pool_clears_on_acquire() {
        let pool = BufferPool::new(16);
        let mut buf = pool.acquire();
        buf.extend_from_slice(b"hello");
        pool.release(buf);
        let buf2 = pool.acquire();
        assert!(buf2.is_empty());
    }
}
