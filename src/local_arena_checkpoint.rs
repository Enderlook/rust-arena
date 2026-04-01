use std::ptr::NonNull;

use crate::{Arena, LocalArena, SharedArena, chunk::ChunkPtr};

/// Represent a checkpoint in a [crate::LocalArena] which can be rollbacked.
///
/// If the checkpoint is drop, then the results are commited, you must use [Self::rollback()] to prevent it.
pub struct LocalArenaCheckpoint<'a> {
    source: Source<'a>,
    checkpoint_chunk: ChunkPtr,
    checkpoint_bump_ptr: NonNull<u8>,
}

enum Source<'a> {
    Owner(&'a mut LocalArena),
    Borrow(&'a mut LocalArenaCheckpoint<'a>),
}

impl<'a> LocalArenaCheckpoint<'a> {
    /// Creates a new checkpoint.
    pub fn make_checkpoint(&'a mut self) -> LocalArenaCheckpoint<'a> {
        let arena = self.get_arena();
        let chunk = arena.get_chunk();
        LocalArenaCheckpoint {
            source: Source::Borrow(self),
            checkpoint_chunk: chunk,
            checkpoint_bump_ptr: unsafe { chunk.as_ref() }.header.current_bump_ptr,
        }
    }

    /// Gets the shared arena where it belongs to.
    pub fn get_shared(&self) -> SharedArena {
        self.get_arena().get_shared()
    }

    /// Rollback all allocations done
    pub fn rollback(&mut self) {
        let chunk = self.checkpoint_chunk;
        let bump_ptr = self.checkpoint_bump_ptr;
        self.get_arena_mut().rollback_to(chunk, bump_ptr);
    }

    pub(crate) fn make_checkpoint_owner(arena: &'a mut LocalArena) -> LocalArenaCheckpoint<'a> {
        let chunk = arena.get_chunk();
        LocalArenaCheckpoint {
            source: Source::Owner(arena),
            checkpoint_chunk: chunk,
            checkpoint_bump_ptr: unsafe { chunk.as_ref() }.header.current_bump_ptr,
        }
    }

    #[inline(always)]
    fn get_arena(&self) -> &LocalArena {
        match &self.source {
            Source::Owner(e) => e,
            Source::Borrow(e) => e.get_arena(),
        }
    }

    #[inline(always)]
    fn get_arena_mut(&mut self) -> &mut LocalArena {
        match &mut self.source {
            Source::Owner(e) => e,
            Source::Borrow(e) => e.get_arena_mut(),
        }
    }
}


impl<'a> Arena for LocalArenaCheckpoint<'a> {
    #[inline(always)]
    fn try_alloc_layout(&self, layout: std::alloc::Layout) -> Result<crate::Box<'_, [std::mem::MaybeUninit<u8>]>, crate::AllocError> {
        self.get_arena().try_alloc_layout(layout)
    }

    #[inline(always)]
    fn try_alloc_remaining_dst_with_builder<B: crate::BuilderDST>(&self, builder: B) -> Result<(crate::Box<'_, [std::mem::MaybeUninit<u8>]>, crate::WriteElementState), crate::CancellationError> {
        self.get_arena().try_alloc_remaining_dst_with_builder(builder)
    }

    #[inline(always)]
    fn reset(&mut self) {
        self.get_arena_mut().reset()
    }

    #[inline(always)]
    fn allocation_info(&self) -> crate::AllocationInfo {
        self.get_arena().allocation_info()
    }

    #[inline(always)]
    fn remaining_chunk_capacity(&self) -> usize {
        self.get_arena().remaining_chunk_capacity()
    }

    #[inline(always)]
    fn try_alloc_remaining_dst_with_layout(&self, header_layout: std::alloc::Layout, element_layout: std::alloc::Layout, elements_len: std::ops::Range<usize>) -> Option<(crate::Box<'_, [std::mem::MaybeUninit<u8>]>, usize)> {
        self.get_arena().try_alloc_remaining_dst_with_layout(header_layout, element_layout, elements_len)
    }

    #[inline(always)]
    fn try_alloc_remaining_slice_with_layout(&self, element_layout: std::alloc::Layout, elements_len: std::ops::Range<usize>) -> Option<(crate::Box<'_, [std::mem::MaybeUninit<u8>]>, usize)> {
        self.get_arena().try_alloc_remaining_slice_with_layout(element_layout, elements_len)
    }

    #[inline(always)]
    fn alloc_remaining_slice_with_layout(&self, element_layout: std::alloc::Layout, maximum_len: usize) -> (crate::Box<'_, [std::mem::MaybeUninit<u8>]>, usize) {
        self.get_arena().alloc_remaining_slice_with_layout(element_layout, maximum_len)
    }

    #[inline(always)]
    fn alloc_remaining_slice_from_iter_with_order<T: IntoIterator>(&self, iter: T, inserting_order: crate::InsertingOrder) -> (crate::Box<'_, [T::Item]>, Option<T::IntoIter>) {
        self.get_arena().alloc_remaining_slice_from_iter_with_order(iter, inserting_order)
    }

    #[inline(always)]
    fn alloc_remaining_slice_from_iter<T: IntoIterator>(&self, iter: T) -> (crate::Box<'_, [T::Item]>, Option<T::IntoIter>) {
        self.get_arena().alloc_remaining_slice_from_iter(iter)
    }

    #[inline(always)]
    fn try_alloc<T>(&self, value: T) -> Result<crate::Box<'_, T>, crate::AllocError> {
        self.get_arena().try_alloc(value)
    }

    #[cfg(feature = "clone_to_uninit")]
    #[inline(always)]
    fn try_alloc_from_clone<T: std::clone::CloneToUninit + ?Sized>(&self, value: &T) -> Result<crate::Box<'_, T>, crate::AllocError> {
        self.get_arena().try_alloc_from_clone(value)
    }

    #[inline(always)]
    fn try_alloc_slice_clone<T: Clone>(&self, slice: &[T]) -> Result<crate::Box<'_, [T]>, crate::AllocError> {
        self.get_arena().try_alloc_slice_clone(slice)
    }

    #[inline(always)]
    fn try_alloc_slice_copy<T: Copy>(&self, slice: &[T]) -> Result<crate::Box<'_, [T]>, crate::AllocError> {
        self.get_arena().try_alloc_slice_copy(slice)
    }

    #[inline(always)]
    fn try_alloc_slice_fill_clone<T: Clone>(&self, len: usize, value: &T) -> Result<crate::Box<'_, [T]>, crate::AllocError> {
        self.get_arena().try_alloc_slice_fill_clone(len, value)
    }

    #[inline(always)]
    fn try_alloc_slice_fill_copy<T: Copy>(&self, len: usize, value: &T) -> Result<crate::Box<'_, [T]>, crate::AllocError> {
        self.get_arena().try_alloc_slice_fill_copy(len, value)
    }

    #[inline(always)]
    fn try_alloc_slice_fill_default<T: Default>(&self, len: usize) -> Result<crate::Box<'_, [T]>, crate::AllocError> {
        self.get_arena().try_alloc_slice_fill_default(len)
    }

    #[inline(always)]
    fn try_alloc_slice_fill_iter<T>(&self, iter: T) -> Result<crate::Box<'_, [T::Item]>, crate::AllocError>
        where
            T: IntoIterator,
            T::IntoIter: ExactSizeIterator {
        self.get_arena().try_alloc_slice_fill_iter(iter)
    }

    #[inline(always)]
    fn try_alloc_slice_fill_with<T>(&self, len: usize, f: impl FnMut(usize) -> T) -> Result<crate::Box<'_, [T]>, crate::AllocError> {
        self.get_arena().try_alloc_slice_fill_with(len, f)
    }

    #[inline(always)]
    fn try_alloc_slice<T>(&self, len: usize) -> Result<crate::Box<'_, [std::mem::MaybeUninit<T>]>, crate::AllocError> {
        self.get_arena().try_alloc_slice(len)
    }

    #[inline(always)]
    fn try_alloc_remaining_slice<T>(&self, range_len: std::ops::Range<usize>) -> Option<crate::Box<'_, [std::mem::MaybeUninit<T>]>> {
        self.get_arena().try_alloc_remaining_slice(range_len)
    }

    #[inline(always)]
    fn alloc_slice_from_remaining<T>(&self, maximum_len: usize) -> crate::Box<'_, [std::mem::MaybeUninit<T>]> {
        self.get_arena().alloc_slice_from_remaining(maximum_len)
    }

    #[inline(always)]
    fn try_alloc_str(&self, str: &str) -> Result<crate::Box<'_, str>, crate::AllocError> {
        self.get_arena().try_alloc_str(str)
    }

    #[inline(always)]
    fn try_alloc_uninit<T>(&self) -> Result<crate::Box<'_, std::mem::MaybeUninit<T>>, crate::AllocError> {
        self.get_arena().try_alloc_uninit()
    }

    #[inline(always)]
    fn try_alloc_with<T>(&self, f: impl FnOnce() -> T) -> Result<crate::Box<'_, T>, crate::AllocError> {
        self.get_arena().try_alloc_with(f)
    }
}