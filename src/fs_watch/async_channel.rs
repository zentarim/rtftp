use std::any::type_name;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt::{Debug, Formatter};
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

pub(super) fn new<T>() -> (TX<T>, RX<T>) {
    let shared_queue = Rc::new(RefCell::new(SharedQueue::new()));
    (
        TX {
            shared_queue: shared_queue.clone(),
        },
        RX { shared_queue },
    )
}

struct SharedQueue<T> {
    queue: VecDeque<T>,
    waker: Option<Waker>,
}

impl<T> SharedQueue<T> {
    fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            waker: None,
        }
    }

    fn push(&mut self, item: T) {
        self.queue.push_back(item);
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }

    fn pop_nowait(&mut self) -> Result<T, QueueError> {
        if let Some(value) = self.queue.pop_front() {
            Ok(value)
        } else {
            Err(QueueError::NoData)
        }
    }
    fn register_waker(&mut self, waker: &Waker) {
        self.waker = Some(waker.clone());
    }
}

#[derive(Debug)]
enum QueueError {
    NoData,
}

pub(super) struct TX<T> {
    shared_queue: Rc<RefCell<SharedQueue<T>>>,
}

impl<T> TX<T> {
    pub(super) fn push(&mut self, value: T) {
        self.shared_queue.borrow_mut().push(value);
    }
}

impl<T> Debug for TX<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "TX<{}>", type_name::<T>())
    }
}

pub(super) struct RX<T> {
    shared_queue: Rc<RefCell<SharedQueue<T>>>,
}

impl<T> RX<T> {
    pub(super) fn next(&self) -> impl Future<Output = T> {
        _Future {
            shared_queue: self.shared_queue.clone(),
        }
    }
}

impl<T> Debug for RX<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "RX<{}>", type_name::<T>())
    }
}

struct _Future<T> {
    shared_queue: Rc<RefCell<SharedQueue<T>>>,
}

impl<T> Future for _Future<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut shared_queue = self.shared_queue.borrow_mut();
        match shared_queue.pop_nowait() {
            Ok(item) => Poll::Ready(item),
            Err(QueueError::NoData) => {
                shared_queue.register_waker(cx.waker());
                Poll::Pending
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::new;
    use std::time::Duration;
    use tokio::task::LocalSet;

    #[tokio::test(flavor = "current_thread")]
    async fn test_queue() {
        let arbitrary_values = vec![67, 78, 31];
        let (mut tx, rx) = new::<usize>();
        tx.push(arbitrary_values[0]);
        tx.push(arbitrary_values[1]);
        tx.push(arbitrary_values[2]);
        assert_eq!(rx.next().await, arbitrary_values[0]);
        assert_eq!(rx.next().await, arbitrary_values[1]);
        assert_eq!(rx.next().await, arbitrary_values[2]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_queue_wait() {
        let arbitrary_value = 78;
        let (mut tx, rx) = new::<usize>();
        let local = LocalSet::new();
        local.spawn_local(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            tx.push(arbitrary_value);
        });
        let next = local.spawn_local(async move { rx.next().await });
        local.await;
        assert_eq!(next.await.unwrap(), arbitrary_value);
    }
}
