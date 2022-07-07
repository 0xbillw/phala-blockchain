use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;
use std::sync::{Arc, Weak};
use std::time::Instant;

use rbtree::RBTree;
use thiserror::Error;
use tokio::sync::oneshot::{channel, Receiver, Sender};
use tokio::sync::Mutex;

pub type VirtualTime = u128;

pub trait FlowIdType: Clone + Send + Eq + Hash + Debug + 'static {}
impl<T: Clone + Send + Eq + Hash + Debug + 'static> FlowIdType for T {}

#[derive(Clone)]
pub struct FairQueue<FlowId: FlowIdType> {
    inner: Arc<Mutex<FairQueueInner<FlowId>>>,
}

#[derive(Error, Debug)]
pub enum AcquireError {
    #[error("fair queue overloaded")]
    Overloaded,
    #[error("canceled while acquiring slot from the fair queue")]
    Canceled,
}

impl<FlowId: FlowIdType> FairQueue<FlowId> {
    pub fn new(backlog_cap: usize, depth: u32) -> Self {
        Self {
            inner: Arc::new_cyclic(|weak_inner| {
                Mutex::new(FairQueueInner::new(backlog_cap, depth, weak_inner.clone()))
            }),
        }
    }

    pub async fn acquire(
        &self,
        flow_id: FlowId,
        weight: u32,
    ) -> Result<ServingGuard<FlowId>, AcquireError> {
        let rx = self.inner.lock().await.acquire(flow_id, weight)?;
        rx.await.or(Err(AcquireError::Canceled))
    }
}

#[derive(Default)]
struct Flow {
    previous_finish_tag: VirtualTime,
    cost_avg: VirtualTime,
}

struct Request<FlowId: FlowIdType> {
    flow_id: FlowId,
    start_tag: VirtualTime,
    start_signal: Sender<ServingGuard<FlowId>>,
}

pub struct ServingGuard<FlowId: FlowIdType> {
    queue: FairQueue<FlowId>,
    flow_id: FlowId,
    start_time: Instant,
}

impl<FlowId: FlowIdType> Drop for ServingGuard<FlowId> {
    fn drop(&mut self) {
        let cost = self.start_time.elapsed().as_micros() as VirtualTime;
        let flow_id = self.flow_id.clone();
        let queue = self.queue.clone();
        // According to the doc of `spawn`:
        // There is no guarantee that a spawned task will execute to completion.
        // When a runtime is shutdown, all outstanding tasks are dropped,
        // regardless of the lifecycle of that task.
        //
        // The queue slot would leak if the current runtime shutdown unexpectly.
        // However, we currently only use this queue inside the contect of rocket runtime.
        // So it could not be a big problem.
        //
        // This can be solved by using std::sync::Mutex instead of the tokio::sync::Mutex.
        // The drawback is
        tokio::task::spawn(async move {
            queue.inner.lock().await.release(&flow_id, cost);
        });
    }
}

struct FairQueueInner<FlowId: FlowIdType> {
    weak_self: Weak<Mutex<FairQueueInner<FlowId>>>,
    flows: HashMap<FlowId, Flow>,
    backlog: RBTree<VirtualTime, Request<FlowId>>,
    backlog_cap: usize,
    depth: u32,
    serving: u32,
    virtual_time: VirtualTime,
}

unsafe impl<T: FlowIdType> Send for FairQueueInner<T> {}

impl<FlowId: FlowIdType> FairQueueInner<FlowId> {
    fn new(backlog_cap: usize, depth: u32, weak_self: Weak<Mutex<FairQueueInner<FlowId>>>) -> Self {
        Self {
            weak_self,
            flows: HashMap::new(),
            backlog: RBTree::new(),
            backlog_cap,
            depth,
            serving: 0,
            virtual_time: 0,
        }
    }

    fn acquire(
        &mut self,
        flow_id: FlowId,
        weight: u32,
    ) -> Result<Receiver<ServingGuard<FlowId>>, AcquireError> {
        let flow = self.flows.entry(flow_id.clone()).or_insert(Flow::default());

        let start_tag = self.virtual_time.max(flow.previous_finish_tag);
        let cost = flow.cost_avg / weight as VirtualTime;
        let finish_tag = start_tag + cost.max(1);
        flow.previous_finish_tag = finish_tag;

        if self.backlog.len() >= self.backlog_cap {
            let (max_start_tag, _) = self
                .backlog
                .get_last()
                .expect("Get the latest request from non-empty backlog should not fail");
            if start_tag >= *max_start_tag {
                return Err(AcquireError::Overloaded);
            } else {
                // Drop the previous low priority request. This would cancel the corresponding
                // `async acquire`.
                let _ = self.backlog.pop_last();
            }
        }

        let (tx, rx) = channel();

        let request = Request {
            flow_id,
            start_tag,
            start_signal: tx,
        };

        if self.serving < self.depth {
            self.dispatch(request);
        } else {
            self.backlog.insert(start_tag, request);
        }

        Ok(rx)
    }

    fn release(&mut self, flow: &FlowId, actual_cost: VirtualTime) {
        if let Some(flow) = self.flows.get_mut(flow) {
            flow.cost_avg = (flow.cost_avg * 4 + actual_cost) / 5;
        }
        self.serving -= 1;
        self.try_pickup_next();
    }

    fn try_pickup_next(&mut self) {
        if let Some((_, request)) = self.backlog.pop_first() {
            self.dispatch(request)
        }
    }

    fn dispatch(&mut self, request: Request<FlowId>) {
        self.serving += 1;
        self.virtual_time = request.start_tag;
        let guard = ServingGuard {
            queue: FairQueue {
                inner: self
                    .weak_self
                    .upgrade()
                    .expect("fair queue: Failed to upgrade weak self"),
            },
            flow_id: request.flow_id,
            start_time: Instant::now(),
        };

        // If the receiver side has been dropped, the ServingGuard would be dropped here
        // and would further try to pickup next request.
        let _ = request.start_signal.send(guard);
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use tokio::sync::mpsc;

    fn spawn_task(
        q: FairQueue<u32>,
        flow_id: u32,
        weight: u32,
        cost: u32,
        iter: usize,
        emit: mpsc::Sender<(u32, usize, bool)>,
    ) {
        for i in 0..iter {
            let emit = emit.clone();
            let q = q.clone();
            tokio::spawn(async move {
                let guard = q.acquire(flow_id, weight).await;
                emit.send((flow_id, i, guard.is_ok())).await.unwrap();
                sleep_ms(cost as _).await;
            });
        }
    }

    async fn sleep_ms(t: u64) {
        tokio::time::sleep(std::time::Duration::from_millis(t)).await;
    }

    #[tokio::test]
    async fn test_eq_cost_eq_weight_normal() {
        let queue = FairQueue::new(15, 2);
        let (tx, mut rx) = mpsc::channel(1);

        spawn_task(queue.clone(), 1, 1, 100, 5, tx.clone());
        spawn_task(queue.clone(), 2, 1, 100, 5, tx.clone());
        spawn_task(queue.clone(), 3, 1, 100, 5, tx.clone());

        drop(tx);
        let mut order = vec![];
        loop {
            match rx.recv().await {
                Some(v) => order.push(v),
                None => break,
            };
        }
        assert_eq!(
            order,
            vec![
                (1, 0, true),
                (1, 1, true),
                (2, 0, true),
                (3, 0, true),
                (1, 2, true),
                (2, 1, true),
                (3, 1, true),
                (1, 3, true),
                (2, 2, true),
                (3, 2, true),
                (1, 4, true),
                (2, 3, true),
                (3, 3, true),
                (2, 4, true),
                (3, 4, true),
            ]
        );
    }

    #[tokio::test]
    async fn test_eq_cost_eq_weight_overload() {
        let queue = FairQueue::new(10, 2);
        let (tx, mut rx) = mpsc::channel(1);

        spawn_task(queue.clone(), 1, 1, 100, 5, tx.clone());
        sleep_ms(10).await;
        spawn_task(queue.clone(), 2, 1, 100, 5, tx.clone());
        sleep_ms(10).await;
        spawn_task(queue.clone(), 3, 1, 100, 5, tx.clone());

        drop(tx);
        let mut order = vec![];
        loop {
            match rx.recv().await {
                Some(v) => order.push(v),
                None => break,
            };
        }
        order.sort();
        assert_eq!(
            order,
            vec![
                (1, 0, true),
                (1, 1, true),
                (1, 2, true),
                (1, 3, true),
                (1, 4, true),
                (2, 0, true),
                (2, 1, true),
                (2, 2, true),
                (2, 3, true),
                (2, 4, false),
                (3, 0, true),
                (3, 1, true),
                (3, 2, true),
                (3, 3, false),
                (3, 4, false),
            ]
        );
    }

    #[tokio::test]
    async fn test_ne_cost_eq_weight_normal() {
        let queue = FairQueue::new(30, 2);
        // round 1, warm up
        for _ in 0..5 {
            let (tx, mut rx) = mpsc::channel(1);

            spawn_task(queue.clone(), 1, 1, 300, 5, tx.clone());
            spawn_task(queue.clone(), 2, 1, 200, 5, tx.clone());
            spawn_task(queue.clone(), 3, 1, 100, 5, tx.clone());

            drop(tx);
            loop {
                if rx.recv().await.is_none() {
                    break;
                };
            }
        }
        // round 2
        {
            let (tx, mut rx) = mpsc::channel(1);

            spawn_task(queue.clone(), 1, 1, 300, 10, tx.clone());
            sleep_ms(10).await;
            spawn_task(queue.clone(), 2, 1, 200, 10, tx.clone());
            sleep_ms(10).await;
            spawn_task(queue.clone(), 3, 1, 100, 10, tx.clone());

            drop(tx);
            let mut order = vec![];
            loop {
                match rx.recv().await {
                    Some(v) => order.push(v),
                    None => break,
                };
            }
            assert_eq!(
                order,
                vec![
                    (1, 0, true),
                    (1, 1, true),
                    (2, 0, true),
                    (3, 0, true),
                    (3, 1, true),
                    (2, 1, true),
                    (3, 2, true),
                    (1, 2, true),
                    (3, 3, true),
                    (2, 2, true),
                    (3, 4, true),
                    (3, 5, true),
                    (1, 3, true),
                    (2, 3, true),
                    (3, 6, true),
                    (3, 7, true),
                    (2, 4, true),
                    (3, 8, true),
                    (1, 4, true),
                    (3, 9, true),
                    (2, 5, true),
                    (1, 5, true),
                    (2, 6, true),
                    (2, 7, true),
                    (1, 6, true),
                    (2, 8, true),
                    (1, 7, true),
                    (2, 9, true),
                    (1, 8, true),
                    (1, 9, true),
                ]
            );
        }
    }

    #[tokio::test]
    async fn test_ne_cost_ne_weight_normal() {
        let queue = FairQueue::new(30, 2);
        // round 1, warm up
        for _ in 0..5 {
            let (tx, mut rx) = mpsc::channel(1);

            spawn_task(queue.clone(), 1, 1, 300, 5, tx.clone());
            spawn_task(queue.clone(), 2, 1, 200, 5, tx.clone());
            spawn_task(queue.clone(), 3, 1, 100, 5, tx.clone());

            drop(tx);
            loop {
                if rx.recv().await.is_none() {
                    break;
                };
            }
        }
        // round 2
        {
            let (tx, mut rx) = mpsc::channel(1);

            spawn_task(queue.clone(), 1, 3, 300, 10, tx.clone());
            sleep_ms(10).await;
            spawn_task(queue.clone(), 2, 2, 200, 10, tx.clone());
            sleep_ms(10).await;
            spawn_task(queue.clone(), 3, 1, 100, 10, tx.clone());

            drop(tx);
            let mut order = vec![];
            loop {
                match rx.recv().await {
                    Some(v) => order.push(v),
                    None => break,
                };
            }
            assert_eq!(
                order,
                vec![
                    (1, 0, true),
                    (1, 1, true),
                    (2, 0, true),
                    (3, 0, true),
                    (1, 2, true),
                    (2, 1, true),
                    (3, 1, true),
                    (1, 3, true),
                    (2, 2, true),
                    (3, 2, true),
                    (1, 4, true),
                    (2, 3, true),
                    (3, 3, true),
                    (1, 5, true),
                    (2, 4, true),
                    (3, 4, true),
                    (1, 6, true),
                    (2, 5, true),
                    (3, 5, true),
                    (1, 7, true),
                    (2, 6, true),
                    (3, 6, true),
                    (1, 8, true),
                    (2, 7, true),
                    (3, 7, true),
                    (1, 9, true),
                    (2, 8, true),
                    (3, 8, true),
                    (2, 9, true),
                    (3, 9, true),
                ]
            );
        }
    }
}
