use core::{mem::MaybeUninit, num::NonZeroUsize, ops::Range, ptr, slice};

/// Basic ring buffer trait.
///
/// Provides status methods and access to underlying memory.
pub trait RingBufferBase<T> {
    /// Returns underlying raw ring buffer memory as slice.
    ///
    /// # Safety
    ///
    /// All operations on this data must cohere with the counter.
    ///
    /// *Accessing raw data is extremely unsafe.*
    /// It is recommended to use [`Consumer::as_slices()`] and [`Producer::free_space_as_slices()`] instead.
    #[allow(clippy::mut_from_ref)]
    unsafe fn data(&self) -> &mut [MaybeUninit<T>];

    /// Capacity of the ring buffer.
    ///
    /// It is constant during the whole ring buffer lifetime.
    fn capacity(&self) -> NonZeroUsize;

    /// Head position.
    fn head(&self) -> usize;

    /// Tail position.
    fn tail(&self) -> usize;

    #[inline]
    /// Modulus for `head` and `tail` values.
    ///
    /// Equals to `2 * len`.
    fn modulus(&self) -> NonZeroUsize {
        unsafe { NonZeroUsize::new_unchecked(2 * self.capacity().get()) }
    }

    /// The number of items stored in the buffer at the moment.
    fn occupied_len(&self) -> usize {
        let modulus = self.modulus();
        (modulus.get() + self.tail() - self.head()) % modulus
    }

    /// The number of vacant places in the buffer at the moment.
    fn vacant_len(&self) -> usize {
        let modulus = self.modulus();
        (modulus.get() + self.head() - self.tail() - self.capacity().get()) % modulus
    }

    /// Checks if the occupied range is empty.
    fn is_empty(&self) -> bool {
        self.head() == self.tail()
    }

    /// Checks if the vacant range is empty.
    fn is_full(&self) -> bool {
        self.vacant_len() == 0
    }
}

/// Ring buffer read end.
///
/// Provides reading mechanism and access to occupied memory.
pub trait RingBufferRead<T>: RingBufferBase<T> {
    /// Sets the new **head** position.
    ///
    /// # Safety
    ///
    /// This call must cohere with ring buffer data modification.
    ///
    /// It is recomended to use `Self::advance_head` instead.
    unsafe fn set_head(&self, value: usize);

    /// Move **head** position by `count` items forward.
    ///
    /// # Safety
    ///
    /// First `count` items in occupied area must be **initialized** before this call.
    ///
    /// *In debug mode panics if `count` is greater than number of items in the ring buffer.*
    unsafe fn advance_head(&self, count: usize) {
        debug_assert!(count <= self.occupied_len());
        self.set_head((self.head() + count) % self.modulus());
    }

    /// Returns a pair of slices which contain, in order, the occupied cells in the ring buffer.
    ///
    /// All items in slices are guaranteed to be **initialized**.
    ///
    /// *The slices may not include items pushed to the buffer by the concurring producer right after this call.*
    fn occupied_ranges(&self) -> (Range<usize>, Range<usize>) {
        let head = self.head();
        let tail = self.tail();
        let len = self.capacity();

        let (head_div, head_mod) = (head / len, head % len);
        let (tail_div, tail_mod) = (tail / len, tail % len);

        if head_div == tail_div {
            (head_mod..tail_mod, 0..0)
        } else {
            (head_mod..len.get(), 0..tail_mod)
        }
    }

    /// Provides a direct mutable access to the ring buffer occupied memory.
    ///
    /// Returns a pair of slices of stored items, the second one may be empty.
    /// Elements with lower indices in slice are older. First slice contains older items that second one.
    ///
    /// # Safety
    ///
    /// All items are initialized. Elements must be removed starting from the beginning of first slice.
    /// When all items are removed from the first slice then items must be removed from the beginning of the second slice.
    ///
    /// *This method must be followed by [`Self::advance`] call with the number of items being removed previously as argument.*
    /// *No other mutating calls allowed before that.*
    unsafe fn occupied_slices(&self) -> (&mut [MaybeUninit<T>], &mut [MaybeUninit<T>]) {
        let ranges = self.occupied_ranges();
        let ptr = self.data().as_mut_ptr();
        (
            slice::from_raw_parts_mut(ptr.add(ranges.0.start), ranges.0.len()),
            slice::from_raw_parts_mut(ptr.add(ranges.1.start), ranges.1.len()),
        )
    }

    /// Removes all items from the buffer and safely drops them.
    ///
    /// If there is concurring producer activity then the buffer may be not empty after this call.
    ///
    /// Returns the number of deleted items.
    ///
    /// # Safety
    ///
    /// Must not be called concurrently.
    unsafe fn clear(&self) -> usize {
        let (left, right) = self.occupied_slices();
        let count = left.len() + right.len();
        for elem in left.iter_mut().chain(right.iter_mut()) {
            ptr::drop_in_place(elem.as_mut_ptr());
        }
        self.advance_head(count);
        count
    }
}

/// Ring buffer write end.
///
/// Provides writing mechanism and access to vacant memory.
pub trait RingBufferWrite<T>: RingBufferBase<T> {
    /// Sets the new **tail** position.
    ///
    /// # Safety
    ///
    /// This call must cohere with ring buffer data modification.
    ///
    /// It is recomended to use `Self::advance_tail` instead.
    unsafe fn set_tail(&self, value: usize);

    /// Move **tail** position by `count` items forward.
    ///
    /// # Safety
    ///
    /// First `count` items in vacant area must be **de-initialized** (dropped) before this call.
    ///
    /// *In debug mode panics if `count` is greater than number of vacant places in the ring buffer.*
    unsafe fn advance_tail(&self, count: usize) {
        debug_assert!(count <= self.vacant_len());
        self.set_tail((self.tail() + count) % self.modulus());
    }

    /// Returns a pair of slices which contain, in order, the vacant cells in the ring buffer.
    ///
    /// All items in slices are guaranteed to be *un-initialized*.
    ///
    /// *The slices may not include cells freed by the concurring consumer right after this call.*
    fn vacant_ranges(&self) -> (Range<usize>, Range<usize>) {
        let head = self.head();
        let tail = self.tail();
        let len = self.capacity();

        let (head_div, head_mod) = (head / len, head % len);
        let (tail_div, tail_mod) = (tail / len, tail % len);

        if head_div == tail_div {
            (tail_mod..len.get(), 0..head_mod)
        } else {
            (tail_mod..head_mod, 0..0)
        }
    }

    /// Provides a direct access to the ring buffer vacant memory.
    /// Returns a pair of slices of uninitialized memory, the second one may be empty.
    ///
    /// # Safety
    ///
    /// Vacant memory is uninitialized. Initialized items must be put starting from the beginning of first slice.
    /// When first slice is fully filled then items must be put to the beginning of the second slice.
    ///
    /// *This method must be followed by `Self::advance_tail` call with the number of items being put previously as argument.*
    /// *No other mutating calls allowed before that.*
    unsafe fn vacant_slices(&self) -> (&mut [MaybeUninit<T>], &mut [MaybeUninit<T>]) {
        let ranges = self.vacant_ranges();
        let ptr = self.data().as_mut_ptr();
        (
            slice::from_raw_parts_mut(ptr.add(ranges.0.start), ranges.0.len()),
            slice::from_raw_parts_mut(ptr.add(ranges.1.start), ranges.1.len()),
        )
    }
}

/// The whole ring buffer.
pub trait RingBuffer<T>: RingBufferRead<T> + RingBufferWrite<T> {}
