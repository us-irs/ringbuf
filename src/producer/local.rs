use crate::{
    consumer::LocalConsumer,
    counter::{Counter, LocalTailCounter},
    transfer::transfer_local,
    utils::write_slice,
};
use core::{mem::MaybeUninit, slice};

#[cfg(feature = "std")]
use crate::utils::slice_assume_init_mut;
#[cfg(feature = "std")]
use core::cmp;
#[cfg(feature = "std")]
use std::io::{self, Read, Write};

/// Producer part of ring buffer.
///
/// Generic over item type, ring buffer container and ring buffer reference.
pub struct LocalProducer<'a, T, S: Counter> {
    data: &'a mut [MaybeUninit<T>],
    counter: LocalTailCounter<'a, S>,
}

impl<'a, T, S: Counter> LocalProducer<'a, T, S> {
    pub(crate) fn new(data: &'a mut [MaybeUninit<T>], counter: LocalTailCounter<'a, S>) -> Self {
        Self { data, counter }
    }

    /// Returns capacity of the ring buffer.
    ///
    /// The capacity of the buffer is constant.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.counter.len().get()
    }

    /// Checks if the ring buffer is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.counter.is_empty()
    }

    /// Checks if the ring buffer is full.
    #[inline]
    pub fn is_full(&self) -> bool {
        self.counter.is_full()
    }

    /// The number of items stored in the buffer.
    pub fn len(&self) -> usize {
        self.counter.occupied_len()
    }

    /// The number of remaining free places in the buffer.
    pub fn remaining(&self) -> usize {
        self.counter.vacant_len()
    }

    /// Provides a direct access to the ring buffer vacant memory.
    /// Returns a pair of slices of uninitialized memory, the second one may be empty.
    ///
    /// # Safety
    ///
    /// Vacant memory is uninitialized. Initialized items must be put starting from the beginning of first slice.
    /// When first slice is fully filled then items must be put to the beginning of the second slice.
    ///
    /// *This method must be followed by `Self::advance` call with the number of items being put previously as argument.*
    /// *No other mutating calls allowed before that.*
    pub unsafe fn free_space_as_slices(
        &mut self,
    ) -> (&mut [MaybeUninit<T>], &mut [MaybeUninit<T>]) {
        let ranges = self.counter.vacant_ranges();
        let ptr = self.data.as_mut_ptr();
        (
            slice::from_raw_parts_mut(ptr.add(ranges.0.start), ranges.0.len()),
            slice::from_raw_parts_mut(ptr.add(ranges.1.start), ranges.1.len()),
        )
    }

    /// Moves `tail` counter by `count` places.
    ///
    /// # Safety
    ///
    /// First `count` items in free space must be initialized.
    pub unsafe fn advance(&mut self, count: usize) {
        self.counter.advance_tail(count);
    }

    /// Appends an item to the ring buffer.
    ///
    /// On failure returns an `Err` containing the item that hasn't been appended.
    pub fn push(&mut self, elem: T) -> Result<(), T> {
        if !self.is_full() {
            unsafe {
                self.free_space_as_slices()
                    .0
                    .get_unchecked_mut(0)
                    .write(elem)
            };
            unsafe { self.advance(1) };
            Ok(())
        } else {
            Err(elem)
        }
    }

    /// Appends items from an iterator to the ring buffer.
    /// Elements that haven't been added to the ring buffer remain in the iterator.
    ///
    /// Returns count of items been appended to the ring buffer.
    pub fn push_iter<I: Iterator<Item = T>>(&mut self, iter: &mut I) -> usize {
        let (left, right) = unsafe { self.free_space_as_slices() };
        let mut count = 0;
        for place in left.iter_mut().chain(right.iter_mut()) {
            match iter.next() {
                Some(elem) => unsafe { place.as_mut_ptr().write(elem) },
                None => break,
            }
            count += 1;
        }
        unsafe { self.advance(count) };
        count
    }

    /// Removes at most `count` items from the consumer and appends them to the producer.
    /// If `count` is `None` then as much as possible items will be moved.
    /// The producer and consumer parts may be of different buffers as well as of the same one.
    ///
    /// On success returns number of items been moved.
    pub fn transfer_from<'b, Sc: Counter>(
        &mut self,
        consumer: &mut LocalConsumer<'b, T, Sc>,
        count: Option<usize>,
    ) -> usize {
        transfer_local(consumer, self, count)
    }
}

impl<'a, T: Copy, S: Counter> LocalProducer<'a, T, S> {
    /// Appends items from slice to the ring buffer.
    /// Elements should be `Copy`.
    ///
    /// Returns count of items been appended to the ring buffer.
    pub fn push_slice(&mut self, elems: &[T]) -> usize {
        let (left, right) = unsafe { self.free_space_as_slices() };
        let count = if elems.len() < left.len() {
            write_slice(&mut left[..elems.len()], elems);
            elems.len()
        } else {
            let (left_elems, elems) = elems.split_at(left.len());
            write_slice(left, left_elems);
            left.len()
                + if elems.len() < right.len() {
                    write_slice(&mut right[..elems.len()], elems);
                    elems.len()
                } else {
                    write_slice(right, &elems[..right.len()]);
                    right.len()
                }
        };
        unsafe { self.advance(count) };
        count
    }
}

#[cfg(feature = "std")]
impl<'a, S: Counter> LocalProducer<'a, u8, S> {
    pub fn read_from<P: Read>(
        &mut self,
        reader: &mut P,
        count: Option<usize>,
    ) -> io::Result<usize> {
        let (left, _) = unsafe { self.free_space_as_slices() };
        let count = cmp::min(count.unwrap_or(left.len()), left.len());
        let left_init = unsafe { slice_assume_init_mut(&mut left[..count]) };

        let read_count = reader.read(left_init)?;
        assert!(read_count <= count);
        unsafe { self.advance(read_count) };
        Ok(read_count)
    }
}

#[cfg(feature = "std")]
impl<'a, S: Counter> Write for LocalProducer<'a, u8, S> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let n = self.push_slice(buffer);
        if n == 0 && !buffer.is_empty() {
            Err(io::ErrorKind::WouldBlock.into())
        } else {
            Ok(n)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
