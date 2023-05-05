use core::{
    future::Future,
    iter::Peekable,
    ops::Deref,
    pin::Pin,
    task::{Context, Poll, Waker},
};
use futures::future::FusedFuture;
//#[cfg(feature = "std")]
//use futures::io::AsyncWrite;
use ringbuf::{
    index::Index,
    storage::Storage,
    traits::{Producer, RingBuffer},
    Prod, Rb,
};

use crate::index::AsyncIndex;
//#[cfg(feature = "std")]
//use std::io;

pub trait AsyncProducer: Producer {
    fn register_waker(&self, waker: &Waker);

    /// Check if the corresponding consumer is closed.
    fn is_closed(&self) -> bool;

    /// Push item to the ring buffer waiting asynchronously if the buffer is full.
    ///
    /// Future returns:
    /// + `Ok` - item successfully pushed.
    /// + `Err(item)` - the corresponding consumer was dropped, item is returned back.
    fn push(&mut self, item: Self::Item) -> PushFuture<'_, Self> {
        PushFuture {
            owner: self,
            item: Some(item),
        }
    }

    /// Push items from iterator waiting asynchronously if the buffer is full.
    ///
    /// Future returns:
    /// + `Ok` - iterator ended.
    /// + `Err(iter)` - the corresponding consumer was dropped, remaining iterator is returned back.
    fn push_iter_all<I: Iterator<Item = Self::Item>>(
        &mut self,
        iter: I,
    ) -> PushIterFuture<'_, Self, I> {
        PushIterFuture {
            owner: self,
            iter: Some(iter.peekable()),
        }
    }

    /// Wait for the buffer to have at least `count` free places for items or to close.
    ///
    /// Panics if `count` is greater than buffer capacity.
    fn wait_vacant(&self, count: usize) -> WaitVacantFuture<'_, Self> {
        debug_assert!(count <= self.capacity().get());
        WaitVacantFuture {
            owner: self,
            count,
            done: false,
        }
    }

    /// Copy slice contents to the buffer waiting asynchronously if the buffer is full.
    ///
    /// Future returns:
    /// + `Ok` - all slice contents are copied.
    /// + `Err(count)` - the corresponding consumer was dropped, number of copied items returned.
    fn push_slice_all<'a: 'b, 'b>(
        &'a mut self,
        slice: &'b [Self::Item],
    ) -> PushSliceFuture<'a, 'b, Self>
    where
        Self::Item: Copy,
    {
        PushSliceFuture {
            owner: self,
            slice: Some(slice),
            count: 0,
        }
    }
}

pub struct PushFuture<'a, A: AsyncProducer> {
    owner: &'a mut A,
    item: Option<A::Item>,
}
impl<'a, A: AsyncProducer> Unpin for PushFuture<'a, A> {}
impl<'a, A: AsyncProducer> FusedFuture for PushFuture<'a, A> {
    fn is_terminated(&self) -> bool {
        self.item.is_none()
    }
}
impl<'a, A: AsyncProducer> Future for PushFuture<'a, A> {
    type Output = Result<(), A::Item>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.owner.register_waker(cx.waker());
        let item = self.item.take().unwrap();
        if self.owner.is_closed() {
            Poll::Ready(Err(item))
        } else {
            match self.owner.try_push(item) {
                Err(item) => {
                    self.item.replace(item);
                    Poll::Pending
                }
                Ok(()) => Poll::Ready(Ok(())),
            }
        }
    }
}

pub struct PushSliceFuture<'a, 'b, A: AsyncProducer>
where
    A::Item: Copy,
{
    owner: &'a mut A,
    slice: Option<&'b [A::Item]>,
    count: usize,
}
impl<'a, 'b, A: AsyncProducer> Unpin for PushSliceFuture<'a, 'b, A> where A::Item: Copy {}
impl<'a, 'b, A: AsyncProducer> FusedFuture for PushSliceFuture<'a, 'b, A>
where
    A::Item: Copy,
{
    fn is_terminated(&self) -> bool {
        self.slice.is_none()
    }
}
impl<'a, 'b, A: AsyncProducer> Future for PushSliceFuture<'a, 'b, A>
where
    A::Item: Copy,
{
    type Output = Result<(), usize>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.owner.register_waker(cx.waker());
        let mut slice = self.slice.take().unwrap();
        if self.owner.is_closed() {
            Poll::Ready(Err(self.count))
        } else {
            let len = self.owner.push_slice(slice);
            slice = &slice[len..];
            self.count += len;
            if slice.is_empty() {
                Poll::Ready(Ok(()))
            } else {
                self.slice.replace(slice);
                Poll::Pending
            }
        }
    }
}

pub struct PushIterFuture<'a, A: AsyncProducer, I: Iterator<Item = A::Item>> {
    owner: &'a mut A,
    iter: Option<Peekable<I>>,
}
impl<'a, A: AsyncProducer, I: Iterator<Item = A::Item>> Unpin for PushIterFuture<'a, A, I> {}
impl<'a, A: AsyncProducer, I: Iterator<Item = A::Item>> FusedFuture for PushIterFuture<'a, A, I> {
    fn is_terminated(&self) -> bool {
        self.iter.is_none() || self.owner.is_closed()
    }
}
impl<'a, A: AsyncProducer, I: Iterator<Item = A::Item>> Future for PushIterFuture<'a, A, I> {
    type Output = bool;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.owner.register_waker(cx.waker());
        let mut iter = self.iter.take().unwrap();
        if self.owner.is_closed() {
            Poll::Ready(false)
        } else {
            self.owner.push_iter(&mut iter);
            if iter.peek().is_none() {
                Poll::Ready(true)
            } else {
                self.iter.replace(iter);
                Poll::Pending
            }
        }
    }
}

pub struct WaitVacantFuture<'a, A: AsyncProducer> {
    owner: &'a A,
    count: usize,
    done: bool,
}
impl<'a, A: AsyncProducer> Unpin for WaitVacantFuture<'a, A> {}
impl<'a, A: AsyncProducer> FusedFuture for WaitVacantFuture<'a, A> {
    fn is_terminated(&self) -> bool {
        self.done
    }
}
impl<'a, A: AsyncProducer> Future for WaitVacantFuture<'a, A> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        assert!(!self.done);
        self.owner.register_waker(cx.waker());
        let closed = self.owner.is_closed();
        if self.count <= self.owner.vacant_len() || closed {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

impl<S: Storage, R: Index, W: Index> AsyncProducer for Rb<S, AsyncIndex<R>, W> {
    fn register_waker(&self, waker: &Waker) {
        unsafe { self.read_index_ref() }.waker.register(waker);
    }
    fn is_closed(&self) -> bool {
        unsafe { self.read_index_ref() }.is_closed()
    }
}

impl<R: Deref> AsyncProducer for Prod<R>
where
    R::Target: RingBuffer + AsyncProducer,
{
    fn register_waker(&self, waker: &Waker) {
        self.base().register_waker(waker);
    }
    fn is_closed(&self) -> bool {
        self.base().is_closed()
    }
}

/*
impl<A: AsyncProducer> Sink<A::Item> for A {
    type Error = ();

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {

        self.register_waker(cx.waker());
        if self.is_closed() {
            Poll::Ready(Err(()))
        } else if self.base.is_full() {
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }
    fn start_send(mut self: Pin<&mut Self>, item: A::Item) -> Result<(), Self::Error> {

        assert!(self.base.push(item).is_ok());
        Ok(())
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {

        // Don't need to be flushed.
        Poll::Ready(Ok(()))
    }
    fn poll_close(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {

        self.close();
        Poll::Ready(Ok(()))
    }
}

#[cfg(feature = "std")]
impl<A: AsyncProducer<Item = u8>> AsyncWrite for A {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {

        self.register_waker(cx.waker());
        if self.is_closed() {
            Poll::Ready(Ok(0))
        } else {
            let count = self.base.push_slice(buf);
            if count == 0 {
                Poll::Pending
            } else {
                Poll::Ready(Ok(count))
            }
        }
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {

        // Don't need to be flushed.
        Poll::Ready(Ok(()))
    }
    fn poll_close(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {

        self.close();
        Poll::Ready(Ok(()))
    }
}
*/
