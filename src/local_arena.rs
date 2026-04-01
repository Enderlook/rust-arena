use std::{alloc::Layout, cell::UnsafeCell, marker::PhantomData, mem::{self, MaybeUninit}, ops::Range, ptr::{self, NonNull}};

use crate::{AllocError, AllocationInfo, Arena, Box, BuilderDST, CancellationError, Chunk, InsertingOrder, SENTINEL, Sentinel, SharedArena, WriteElementState, chunk::{ChunkPtr, DSTInfo}};

use crate::compatibility::*;

/// Represent a thread-local arena.
pub struct LocalArena {
    frontier: UnsafeCell<ChunkPtr>,
    shared: SharedArena,
    phantom: PhantomData<*mut ()>, /* Removes Send + Sync */
}

impl LocalArena {
    pub(crate) fn new(shared: SharedArena) -> Result<Self, AllocError> {
        Ok(Self {
            frontier: UnsafeCell::new(SENTINEL.get_or_try_init(#[cold] || Ok(Sentinel(Chunk::create_chunk(0)?)))?.0),
            shared,
            phantom: PhantomData
        })
    }

    /// Gets the shared arena where it belongs to.
    pub fn get_shared(&self) -> SharedArena {
        self.shared.clone()
    }

    #[cold]
    fn try_grow_and_alloc(&self, layout: Layout) -> Result<Box<'_, [MaybeUninit<u8>]>, AllocError> {
        let len = unsafe { self.frontier.as_mut_unchecked_().as_ref() }.storage.len();
        let size = len.checked_add(1).map(#[inline(always)] |e| e.checked_next_power_of_two()).flatten().unwrap_or(len);

        let sentinel = SENTINEL.get();
        debug_assert!(sentinel.is_some(), "Should have value since this is only called from a constructed LocalArena.");
        let sentinel = unsafe { sentinel.unwrap_unchecked().0 };

        let mut new_chunk = self.shared.get_or_make_chunk(layout, size)?;
        let old_chunk = unsafe { self.frontier.replace_(new_chunk) };
        if !ptr::eq(sentinel.as_ptr(), old_chunk.as_ptr()) {
            unsafe { new_chunk.as_mut() }.header.prev = Some(old_chunk);
        }

        unsafe { (new_chunk.as_mut()).try_alloc(layout) }
            .map(#[inline(always)] |ptr| {
                debug_assert_eq!(ptr.addr().get() % layout.align(), 0, "Invalid alignment.");
                unsafe { Box::from_non_null(ptr.cast::<MaybeUninit<u8>>().cast_slice_(layout.size())) }
            })
    }

    #[cold]
    fn try_get_chunk_and_alloc<'a, 'b, T: 'b, U>(&'a self, fun: impl FnOnce(&'a SharedArena) -> Result<(ChunkPtr, T), U>) -> Result<T, U> {
        match fun(&self.shared) {
            Ok((new_chunk, result)) => {
                let sentinel = SENTINEL.get();
                debug_assert!(sentinel.is_some(), "Should have value since this is only called from a constructed LocalArena.");
                let sentinel = unsafe { sentinel.unwrap_unchecked().0 };

                let old_chunk = unsafe { self.frontier.replace_(new_chunk) };
                if !ptr::eq(sentinel.as_ptr(), old_chunk.as_ptr()) {
                    unsafe { (*new_chunk.as_ptr()).header.prev = Some(old_chunk) };
                }

                Ok(result)
            },
            Err(e) => Err(e),
        }
    }
}

impl Arena for LocalArena {
    #[inline(always)]
    fn try_alloc_layout(&self, layout: Layout) -> Result<Box<'_, [MaybeUninit<u8>]>, AllocError> {
        let chunk = unsafe { self.frontier.as_mut_unchecked_() };
        if let Some(ptr) = unsafe { chunk.as_mut() }.try_alloc(layout).ok() {
            debug_assert_eq!(ptr.addr().get() % layout.align(), 0, "Invalid alignment.");
            Ok(unsafe { Box::from_non_null(ptr.cast::<MaybeUninit<u8>>().cast_slice_(layout.size())) })
        } else {
            self.try_grow_and_alloc(layout)
        }
    }

    #[inline(always)]
    fn try_alloc_remaining_dst_with_builder<B: BuilderDST>(&self, mut builder: B) -> Result<(Box<'_, [MaybeUninit<u8>]>, WriteElementState), CancellationError> {
        return if let Some(info) = DSTInfo::new(&builder) {
            let chunk = unsafe { self.frontier.as_mut_unchecked_().as_mut() };
            match chunk.try_alloc_remaining_dst_with_builder(&info) {
                Ok(e) => e.finish(&mut builder),
                Err(()) => slow(self, &mut builder, &info),
            }
        } else {
            Err(CancellationError::CancelledBeforeWrite)
        };

        #[cold]
        fn slow<'a>(this: &'a LocalArena, builder: &mut impl BuilderDST, info: &DSTInfo) -> Result<(Box<'a, [MaybeUninit<u8>]>, WriteElementState), CancellationError> {
            match this.shared.try_alloc_remaining_dst_with_builder(builder, &info) {
                Ok((new_chunk, result)) => {
                    let sentinel = SENTINEL.get();
                    debug_assert!(sentinel.is_some(), "Should have value since this is only called from a constructed LocalArena.");
                    let sentinel = unsafe { sentinel.unwrap_unchecked().0 };

                    let old_chunk = unsafe { this.frontier.replace_(new_chunk) };
                    if !ptr::eq(sentinel.as_ptr(), old_chunk.as_ptr()) {
                        unsafe { (*new_chunk.as_ptr()).header.prev = Some(old_chunk) };
                    }

                    Ok(result)
                },
                Err(e) => Err(e),
            }
        }
    }

    #[inline(always)]
    fn try_alloc_remaining_dst_with_layout(&self, header_layout: Layout, element_layout: Layout, mut elements_len: Range<usize>) -> Option<(Box<'_, [MaybeUninit<u8>]>, usize)> {
        elements_len = usize::min(elements_len.start, elements_len.end)..usize::max(elements_len.start, elements_len.end);
        return match unsafe { self.frontier.as_mut_unchecked_().as_mut() }
            .try_alloc_remaining_dst_with_layout(header_layout, element_layout, elements_len.clone()) {
                Some(result) => Some(result),
                None => slow(self, header_layout, element_layout, elements_len),
        };

        #[cold]
        fn slow(this: &LocalArena, header_layout: Layout, element_layout: Layout, elements_len: Range<usize>) -> Option<(Box<'_, [MaybeUninit<u8>]>, usize)> {
            this.try_get_chunk_and_alloc(#[inline(always)] |shared| shared.try_alloc_remaining_dst_layout(header_layout, element_layout, elements_len).ok_or(())).ok()
        }
    }

    #[inline(always)]
    fn try_alloc_remaining_slice_with_layout(&self, element_layout: Layout, mut range_len: Range<usize>) -> Option<(Box<'_, [MaybeUninit<u8>]>, usize)> {
        range_len = usize::min(range_len.start, range_len.end)..usize::max(range_len.start, range_len.end);
        if element_layout.size() == 0 {
            return as_zero(range_len.end);
        }
        return match unsafe { self.frontier.as_mut_unchecked_().as_mut() }
            .try_alloc_remaining_slice_with_layout(element_layout, range_len.clone()) {
                Some(result) => Some(result),
                None => slow(self, element_layout, range_len),
            };

        #[cold]
        fn as_zero<'a>(end: usize) -> Option<(Box<'a, [MaybeUninit<u8>]>, usize)> {
            Some((unsafe { Box::from_non_null(NonNull::dangling().cast_slice_(0)) }, end))
        }

        #[cold]
        fn slow(this: &LocalArena, element_layout: Layout, range_len: Range<usize>) -> Option<(Box<'_, [MaybeUninit<u8>]>, usize)> {
            this.try_get_chunk_and_alloc(#[inline(always)] |shared| shared.try_alloc_remaining_slice_with_layout(element_layout, range_len).ok_or(())).ok()
        }
    }

    #[inline(always)]
    fn alloc_remaining_slice_with_layout(&self, element_layout: Layout, maximum_len: usize) -> (Box<'_, [MaybeUninit<u8>]>, usize) {
        if element_layout.size() == 0{
            return as_zero(maximum_len);
        }
        let result = unsafe { self.frontier.as_mut_unchecked_().as_mut() }
            .alloc_remaining_slice_with_layout(element_layout, maximum_len);
        return if result.1 > 0 || maximum_len == 0 {
            result
        } else {
            slow(self, element_layout, maximum_len)
        };

        #[cold]
        fn as_zero<'a>(maximum_len: usize) -> (Box<'a, [MaybeUninit<u8>]>, usize) {
            (unsafe { Box::from_non_null(NonNull::dangling().cast_slice_(0)) }, maximum_len)
        }

        #[cold]
        fn slow(this: &LocalArena, element_layout: Layout, maximum_len: usize) -> (Box<'_, [MaybeUninit<u8>]>, usize) {
            this.try_get_chunk_and_alloc(#[inline(always)] |shared| shared.alloc_remaining_slice_with_layout(element_layout, maximum_len).ok_or(()))
                .unwrap_or_else(#[inline(always)] |_| as_zero(0))
        }
    }

    #[inline(always)]
    fn alloc_remaining_slice_from_iter_with_order<T: IntoIterator>(&self, iter: T, inserting_order: InsertingOrder) -> (Box<'_, [T::Item]>, Option<T::IntoIter>) {
        // ZST handling.
        if mem::size_of::<T::Item>() == 0 {
            return as_zero(iter);
        }
        let iter = iter.into_iter();
        let (result, remaining) = unsafe { self.frontier.as_mut_unchecked_().as_mut() }.alloc_remaining_slice_from_iter_with_order(iter, inserting_order);
        if result.len() == 0 {
            if let Some(iter) = remaining {
                return slow(self, iter, inserting_order);
            }
        }
        return (result, remaining);

        #[cold]
        fn as_zero<'a, T: IntoIterator>(iter: T) -> (Box<'a, [T::Item]>, Option<T::IntoIter>) {
            let mut iter = iter.into_iter();
            let mut count = 0usize;
            while let Some(_) = iter.next() {
                // There is no point in storing zero-sized values.
                count += 1;
            }
            (unsafe { Box::from_non_null(NonNull::dangling().cast_slice_(count)) }, None)
        }

        #[cold]
        fn slow<T: Iterator>(this: &LocalArena, iter: T, inserting_order: InsertingOrder) -> (Box<'_, [T::Item]>, Option<T>) {
            this.try_get_chunk_and_alloc(#[inline(always)] |shared| shared.alloc_remaining_slice_from_iter_with_order(iter, inserting_order))
                .unwrap_or_else(#[inline(always)] |e| (unsafe { Box::from_non_null(NonNull::dangling().cast_slice_(0)) }, Some(e)))
        }
    }

    fn reset(&mut self) {
        // In order to construct a `LocalArena` we need the value of `SENTINEL`,
        // since this is a instance function, it's guaranteed the `SENTINEL` is constructed.
        let sentinel = SENTINEL.get();
        debug_assert!(sentinel.is_some(), "Should have value since this is only called from a constructed LocalArena.");
        let sentinel = unsafe { sentinel.unwrap_unchecked().0 };
        let chunk = unsafe { self.frontier.replace_(sentinel) };
        if !chunk.are_equal(sentinel) {
            // We only clear if the chunk is not the sentinel,
            // as that one is immutable.
            self.shared.store_chunk(chunk);
        }
    }

    fn allocation_info(&self) -> AllocationInfo {
        let mut chunk = unsafe { self.frontier.as_ref_unchecked_().as_ref() };
        let mut info = AllocationInfo::default();
        loop {
            info.user_bytes += chunk.storage.len() - (chunk.header.current_bump_ptr.as_ptr().addr() - chunk.storage.as_ptr() as usize);
            info.storage_bytes += chunk.storage.len();
            info.total_bytes += Layout::for_value::<Chunk>(&chunk).size();
            chunk = if let Some(chunk) = chunk.header.prev.as_ref() {
                unsafe { chunk.as_ref() }
            } else {
                break;
            }
        }
        info
    }

    #[inline]
    fn remaining_chunk_capacity(&self) -> usize {
        let chunk = unsafe { self.frontier.as_ref_unchecked_().as_ref() };
        let remaining = chunk.header.current_bump_ptr.as_ptr().addr() - chunk.storage.as_ptr().addr();
        remaining
    }
}

impl Drop for LocalArena {
    #[inline(always)]
    fn drop(&mut self) {
        self.reset();
    }
}

#[cfg(test)]
mod tests {
    use crate::{LocalArena, SharedArena, arena_test::arena_test_};

    fn identity(a: LocalArena) -> LocalArena {
        a
    }

    #[test]
    fn shared() {
        let shared = SharedArena::default();
        let arena = shared.make_local();
        let _ = arena.get_shared();
    }

    arena_test_!(identity);
}