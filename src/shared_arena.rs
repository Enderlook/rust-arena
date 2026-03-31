use std::{alloc::{Layout, dealloc}, mem::{self, MaybeUninit}, ops::Range, ptr::{self, NonNull}, sync::Arc};

use parking_lot::Mutex;

use crate::{AllocError, Box, BuilderDST, CancellationError, InsertingOrder, LocalArena, SENTINEL, WriteElementState, chunk::{AllocDSTBuilder, Chunk, ChunkHeader, ChunkPtr, DSTInfo}};

pub(crate) const MINIMUM_ALLOCATED_SIZE: usize = 1024;

/// Represent a shared thread-safe arena.
///
/// Destructors of elements are never run. If you want them run, use [crate::Box] or [crate::Rc].
///
/// The arena is used by creating local arenas wit [Self::make_local] and then using their [crate::Arena] trait implementation.
/// Allocation lifetimes are tied to each local arena.
/// Additionally it supports resetting the local arena while leaving intact other arenas.
#[derive(Clone)]
pub struct SharedArena(Arc<SharedArenaInner>);

#[derive(Default)]
struct SharedArenaInner(Mutex<Option<ChunkPtr>>);

impl Default for SharedArena {
    fn default() -> Self {
        Self(Default::default())
    }
}

impl SharedArena {
    /// Creates a local arena.
    pub fn make_local(&self) -> LocalArena {
        LocalArena::new(self.clone()).unwrap()
    }

    #[inline(always)]
    pub(crate) fn get_or_make_chunk(&self, minimum_layout: Layout, recommended_size_for_allocation: usize) -> Result<ChunkPtr, AllocError> {
        if let Some(chunk) = extract_chunk(&mut *self.0.0.lock(), minimum_layout) {
            return Ok(chunk);
        }
        Self::make_chunk(minimum_layout, recommended_size_for_allocation)
    }

    #[inline(always)]
    pub(crate) fn store_chunk(&self, mut chunk: ChunkPtr) {
        let slot = &mut *self.0.0.lock();
        clean_and_append(&mut chunk, slot.take());
        *slot = Some(chunk);
    }

    #[cold]
    fn make_chunk(minimum_layout: Layout, recommended_size_for_allocation: usize) -> Result<ChunkPtr, AllocError> {
        let minimum_size = min_chunk_size(minimum_layout);
        let mut size = usize::max(minimum_size, usize::max(MINIMUM_ALLOCATED_SIZE, recommended_size_for_allocation));
        loop {
            match Chunk::create_chunk(size) {
                Ok(e) => return Ok(e),
                Err(e) if size == minimum_size => return Err(e),
                _ => {},
            }
            size = usize::max(size / 2, minimum_size);
        }
    }

    #[cold]
    pub(crate) fn try_alloc_remaining_dst_with_builder(&self, builder: &mut impl BuilderDST, info: &DSTInfo) -> Result<(ChunkPtr, (Box<'_, [mem::MaybeUninit<u8>]>, WriteElementState)), CancellationError> {
        let (mut chunk, result) = {
            let root = &mut *self.0.0.lock();
            match extract_chunk_dst_builder(root, builder, info) {
                Some((chunk, result)) => (chunk, result),
                None => return Err(CancellationError::CancelledBeforeWrite),
            }
        };
        match result.finish(builder) {
            Ok(e) => Ok((chunk, e)),
            Err(e) => {
                let new_root = &mut *self.0.0.lock();
                unsafe { chunk.as_mut() }.header.prev = *new_root;
                *new_root = Some(chunk);
                Err(e)
            },
        }
    }

    #[cold]
    pub(crate) fn try_alloc_remaining_dst_layout(&self, header_layout: Layout, element_layout: Layout, elements_len: Range<usize>) -> Option<(ChunkPtr, (Box<'_, [mem::MaybeUninit<u8>]>, usize))> {
        extract_chunk_fun(
            &mut *self.0.0.lock(),
            &mut |chunk| unsafe { (*chunk.as_ptr()).try_alloc_remaining_dst_with_layout(header_layout, element_layout, elements_len.clone()) })
    }

    #[cold]
    pub(crate) fn try_alloc_remaining_slice_with_layout(&self, element_layout: Layout, range_len: Range<usize>) -> Option<(ChunkPtr, (Box<'_, [MaybeUninit<u8>]>, usize))> {
        extract_chunk_fun(
            &mut *self.0.0.lock(),
            &mut |chunk| unsafe { (*chunk.as_ptr()).try_alloc_remaining_slice_with_layout(element_layout, range_len.clone()) })
    }

    #[cold]
    pub(crate) fn alloc_remaining_slice_with_layout(&self, element_layout: Layout, maximum_len: usize) -> Option<(ChunkPtr, (Box<'_, [MaybeUninit<u8>]>, usize))> {
        extract_chunk_fun(
            &mut *self.0.0.lock(),
            &mut |chunk| {
                let result = unsafe { (*chunk.as_ptr()).alloc_remaining_slice_with_layout(element_layout, maximum_len) };
                if result.1 > 0 {
                    Some(result)
                } else {
                    None
                }
            })
    }

    #[cold]
    pub(crate) fn alloc_remaining_slice_from_iter_with_order<T: Iterator>(&self, iter: T, inserting_order: InsertingOrder) -> Result<(ChunkPtr, (Box<'_, [T::Item]>, Option<T>)), T> {
        let mut iter = Some(iter);
        if let Some(result) = extract_chunk_fun(
            &mut *self.0.0.lock(),
            &mut |chunk| {
                let iter_ = iter.take();
                debug_assert!(iter_.is_some(), "Closure was called even after returning a successful value.");
                let (result, remaining) = unsafe { (*chunk.as_ptr()).alloc_remaining_slice_from_iter_with_order(iter_.unwrap_unchecked(), inserting_order) };
                if result.len() == 0 {
                    if let Some(remaining) = remaining {
                        iter = Some(remaining);
                        return None;
                    }
                }
                Some((result, remaining))
            }) {
            Ok(result)
        } else {
            let iter_ = iter.take();
            debug_assert!(iter_.is_some(), "Closure failed despite consuming the iter.");
            Err(unsafe { iter_.unwrap_unchecked() })
        }
    }
}

impl Drop for SharedArenaInner {
    fn drop(&mut self) {
        if let Some(e) = &mut *self.0.lock() {
            let ptr = e.as_non_null().as_ptr();
            unsafe {
                ptr::drop_in_place(ptr);
                dealloc(ptr.cast::<u8>(), Layout::for_value_raw(ptr))
            }
        }
    }
}

#[inline(always)]
fn extract_chunk(chunk: &mut Option<ChunkPtr>, minimum_layout: Layout) -> Option<ChunkPtr> {
    if let Some(value) = chunk.take() {
        let chunk_ptr = value.as_ptr();
        if unsafe { (*chunk_ptr).can_allocate(minimum_layout) } {
            *chunk = unsafe { (*chunk_ptr).header.prev.take() };
            Some(value)
        } else {
            let result = extract_chunk(unsafe { &mut (*chunk_ptr).header.prev }, minimum_layout);
            *chunk = Some(value);
            result
        }
    } else {
        None
    }
}

#[inline(always)]
fn extract_chunk_dst_builder<'a, 'b, 'c>(chunk: &'a mut Option<ChunkPtr>, builder: &'b mut impl BuilderDST, info: &'c DSTInfo) -> Option<(ChunkPtr, AllocDSTBuilder<'c>)> {
    if let Some(value) = chunk.take() {
        let chunk_ptr = value.as_ptr();
        if let Ok(mut e) = unsafe { (*chunk_ptr).try_alloc_remaining_dst_with_builder(info) } {
            *chunk = match &mut e {
                AllocDSTBuilder::Normal { chunk, .. } => chunk,
                AllocDSTBuilder::Rare { chunk, .. } => chunk,
            }.header.prev.take();
            Some((value, e))
        } else {
            let result = extract_chunk_dst_builder(unsafe { &mut (*chunk_ptr).header.prev }, builder, info);
            *chunk = Some(value);
            result
        }
    } else {
        None
    }
}

#[inline(always)]
fn extract_chunk_fun<'a, T>(chunk: &mut Option<ChunkPtr>, fun: &mut impl FnMut(ChunkPtr) -> Option<T>) -> Option<(ChunkPtr, T)> {
    if let Some(value) = chunk.take() {
        if let Some(result) = fun(value) {
            *chunk = unsafe { (*value.as_ptr()).header.prev.take() };
            Some((value, result))
        } else {
            let result = extract_chunk_fun(unsafe { &mut (*value.as_ptr()).header.prev }, fun);
            *chunk = Some(value);
            result
        }
    } else {
        None
    }
}

#[inline(always)]
fn min_chunk_size(layout: Layout) -> usize {
    let align = layout.align();
    let size = layout.size();
    // Round up size to alignment.
    // Layout guarantees that rounding size up to its alignment can't overflow.
    (size + align - 1) & !(align - 1)
}

#[inline(always)]
fn clean_and_append(chunk: &mut ChunkPtr, old_root: Option<ChunkPtr>) {
    unsafe {
        let chunk = chunk.as_ptr();
        debug_assert_ne!(chunk, SENTINEL.get().expect("Should have value since this is only called from a constructed LocalArena.").0.as_ptr());
        let storage_len = ptr::metadata(chunk);
        let storage_start = chunk.cast::<u8>().add(mem::size_of::<ChunkHeader>());
        let storage_end_ptr = storage_start.add(storage_len);
        (*chunk).header.current_bump_ptr = NonNull::new_unchecked(storage_end_ptr);
        let mut tail = chunk as *mut Chunk;
        while let Some(prev) = (*tail).header.prev {
            let prev = prev.as_ptr();
            debug_assert_ne!(prev, SENTINEL.get().expect("Should have value since this is only called from a constructed LocalArena.").0.as_ptr());
            let storage_len = ptr::metadata(prev);
            let storage_start = prev.cast::<u8>().add(mem::size_of::<ChunkHeader>());
            let storage_end_ptr = storage_start.add(storage_len);
            (*prev).header.current_bump_ptr = NonNull::new_unchecked(storage_end_ptr);
            tail = prev as *mut Chunk;
        }
        (*tail).header.prev = old_root;
    }
}