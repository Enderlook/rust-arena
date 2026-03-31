//! This crate is an implementation of a thread-safe arena.
//!
//! [Repository](https://github.com/Enderlook/rust-arena)
//!
//! To use, instantiate a [SharedArena], then call [SharedArena::make_local()] which returns you a thread-local sub arena.
//!
//! [LocalArena] can be reset individually while the other arenas continue.
//! During reset, its internal allocations are send to [SharedArena] to be reused by other [LocalArena] instances.
//!
//! The crate also provides an [Arena] trait to make more arenas.

#![feature(ptr_metadata)]
#![feature(clone_to_uninit)]
#![feature(cast_maybe_uninit)]
#![feature(unsafe_cell_access)]
#![feature(ptr_cast_slice)]
#![feature(once_cell_try)]
#![feature(layout_for_ptr)]
#![feature(slice_ptr_get)]

mod boxed;
mod chunk;
mod local_arena;
mod shared_arena;

#[cfg(test)]
mod arena_test;

use std::{alloc::Layout, clone::CloneToUninit, error::Error, fmt, marker::PhantomData, mem::MaybeUninit, ops::Range, ptr::{self, NonNull}, sync::OnceLock};

pub use boxed::*;
pub use local_arena::*;
pub use shared_arena::*;

use crate::chunk::{Chunk, ChunkPtr};

static SENTINEL: OnceLock<Sentinel> = OnceLock::new();

struct Sentinel(ChunkPtr);

unsafe impl Sync for Sentinel {}
unsafe impl Send for Sentinel {}

/// Error produced when attempting to allocate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllocError {
    /// Allocator ran out of memory.
    OutOfMemory,
    /// The layout is invalid.
    /// This happens if the layout of the requested object is too big,
    /// for example if you request a slice whose len is so big it would produce an invalid layout,
    /// or if you layout is very big, and summed with the internal layout of the backing storage (metadata)
    /// it would produce a so big layout that is invalid.
    InvalidLayout,
}

impl Error for AllocError {}

impl fmt::Display for AllocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfMemory => write!(f, "System run out of memory."),
            Self::InvalidLayout => write!(f, "The layout required to allocate was invalid. This can happen if a too big layout was requested."),
        }
    }
}

/// Information about an allocation content.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct AllocationInfo {
    /// Number of bytes currently allocated by user across all chunks in this arena.
    /// That is, this is the sum of bytes of all the elements allocated by the arena.
    /// This also includes bytes spent for aligning.
    user_bytes: usize,
    /// Number of bytes currently allocated as backing storage across all chunks in this arena.
    /// The difference with [Self::user_bytes] is that if you request an allocation that doesn't fit
    /// in the remaining space of a chunk, a new chunk will be used instead,
    /// discarding the remaining space of the previous one.
    /// This value includes that discarded remaining space.
    /// This value is always equal or bigger than [Self::user_bytes].
    storage_bytes: usize,
    /// Number of bytes currently allocated across as chunks across all chunks in this arena.
    /// The difference with [Self::storage_bytes] is that this value also includes bytes used for metadata of chunks,
    /// rather than only the user-space for allocations.
    /// This value is always bigger than [Self::storage_bytes], unless when `0`, in that case both are `0`.
    total_bytes: usize,
}

/// Determines the order of insertion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertingOrder {
    /// The insertion order must be the same as layered by the user.
    Original,
    /// The insertion order must be the inverse same as layered by the user.
    Reverse,
    /// The insertion order is irrelevant, this is useful for uninitialized or zeroed memory, where order is unnecessary.
    Unspecified,
}

impl Default for InsertingOrder {
    fn default() -> Self {
        Self::Original
    }
}

/// Trait used to construct a dynamically sized type.
pub trait BuilderDST {
    /// Layout of the header part.
    fn header_layout(&self) -> Layout;

    /// Layout of the elements in the tail part.
    fn element_layout(&self) -> Layout;

    /// Informs the builder of a hint about the number of elements it plans to allocate.
    ///
    /// The first number is the minimum number of elements that the builder will require to write to avoid cancellation.
    /// If the remaining capacity of the allocator is lower than this number, it's considered that the builder will cancel,
    /// so the instantiation never takes place to avoid the writes.
    ///
    /// The second number is the maximum number of elements the builder plans to write.
    /// This value may be used by the arena for internal optimizations
    /// and it's not guaranteed that the arena will contains this capacity.
    /// In fact, an arena implementation may completely ignore it.
    ///
    /// Note that these values are hints, and the builder may write fewer or more elements.
    fn elements_hint(&self) -> (usize, Option<usize>);

    /// Order in which elements in the insert part are layered.
    ///
    /// Note that this method doesn't require to be called before [Self::write_element] but may instead be called after those methods,
    /// this is left as an implementation detail of the allocator.
    fn inserting_order(&self) -> InsertingOrder;

    /// Writes the header of the type.
    ///
    /// This method may be called after or before [Self::write_element], depending on the allocator implementation.
    ///
    /// If returns `false`, the allocation in cancelled.
    ///
    /// Note that the specific address of the memory should not be used, as the arena may move the object before returning it.
    fn write_header(&mut self, memory: &mut [u8]) -> bool;

    /// Writes the element of the type.
    ///
    /// This method may be called after or before [Self::write_header], depending on the allocator implementation.
    ///
    /// This function is called until it returns `false`.
    ///
    /// Note that the specific address of the memory should not be used, as the arena may move the object before returning it, or event allocate elements in different regions, and then move then into a single allocation.
    fn write_element(&mut self, memory: &mut [u8]) -> bool;

    /// Executed with the entire memory of the newly constructed type.
    ///
    /// If returns `false`, the allocation in cancelled.
    fn finalizer(&mut self, memory: &mut [u8]) -> bool;

    /// Drops the content written in the memory region of the header.
    ///
    /// This is called when [Self::write_header] was called and the allocation was aborted due to panic
    /// or when [Self::finalizer] returned `false`.
    fn drop_header(&mut self, memory: &mut [u8]);

    /// Drops the content written in the memory region of the element.
    ///
    /// This method is called the same number of times [Self::write_element] was called (minus the one that returned `false`).
    ///
    /// This is called when the allocation was aborted due to panic or when [Self::finalizer] returned `false`.
    fn drop_element(&mut self, memory: &mut [u8]);
}

/// Stores the reason why the allocation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancellationError {
    /// Cancelled without calling [BuilderDST::write_header] nor [BuilderDST::write_element].
    ///
    /// That means there wasn't enough remaining space.
    CancelledBeforeWrite,

    /// Cancelled when by [BuilderDST::write_header].
    CancelledByHeader(WriteElementState),

    /// Canceled when by [BuilderDST::finalizer].
    CancelledByFinalizer(WriteElementState),
}

/// Determines if [BuilderDST::write_element] was caller at least once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteElementState {
    /// [BuilderDST::write_element] was never called.
    NeverStarted,

    /// [BuilderDST::write_element] was called at least once.
    Started {
        /// Number of elements written.
        count: usize,
        /// Determines if the method [BuilderDST::write_element] was run until returning `false`.
        completed: bool,
    }
}

impl WriteElementState {
    /// Gets the write elements count.
    #[inline(always)]
    pub fn count(self) -> usize {
        match self {
            Self::NeverStarted => 0,
            Self::Started { count, .. } => count,
        }
    }
}

/// Represent an arena.
///
/// For allocations of size 0, that is for example zero-sized types or slices of 0 len, the arena is allowed to return several times the same reference.
/// This may be considered a violation of Rust aliasing rules, but since the allocation has no size, it can't be dereferenced anyways.
///
/// Returned allocations can't outlive the arena borrow.
pub trait Arena {
    /// Try to allocate a given layout in the arena.
    /// The error is returned if there layout alignment is above allowed by the arena, layout size is too big or system run out of memory.
    fn try_alloc_layout(&self, layout: Layout) -> Result<Box<'_, [MaybeUninit<u8>]>, AllocError>;

    /// Attempts to allocate a dynamic sized type.
    ///
    /// Returns a result:
    ///  - If `Err(_)`, the allocation couldn't succeed, and the returned enum determines at which point it got cancelled.
    ///    This happens when the [BuilderDST::finalizer], [BuilderDST::write_header] returns `false`,
    ///    if the first number returned by [BuilderDST::elements_hint] is greater than the remaining capacity of the arena,
    ///    or when there wasn't enough space for the header
    /// - If `Ok((_, _))`, the first value is the allocation, and the second determines the number of written elements.
    fn try_alloc_remaining_dst_with_builder<B: BuilderDST>(&self, builder: B) -> Result<(Box<'_, [MaybeUninit<u8>]>, WriteElementState), CancellationError>;

    /// Reset this arena allocator by resetting the pointer of the underlying memory chunks.
    /// We take a mut reference so the borrow checker ensures that all references from this arena are already dropped.
    fn reset(&mut self);

    /// Get information about the allocations and memory used by this arena.
    fn allocation_info(&self) -> AllocationInfo;

    /// Number of bytes remaining in the current chunk (of the current thread if thread-local).
    fn remaining_chunk_capacity(&self) -> usize;

    /// Attempts to allocate a dynamic sized type with the specified header layout and tail with the specified element layout and range of elements.
    ///
    /// Allocation succeed with `Some((_, _))` if there is enough space to allocate the header and the minimum number of elements specified by `elements_len`,
    /// if possible, it will attempt to allocate up to the maximum number in `element_len`.
    /// The first value is the allocation, and the second determines the state of number elements.
    fn try_alloc_remaining_dst_with_layout(&self, header_layout: Layout, element_layout: Layout, elements_len: Range<usize>) -> Option<(Box<'_, [MaybeUninit<u8>]>, usize)> {
        return self.try_alloc_remaining_dst_with_builder(Builder {
            header_layout,
            element_layout,
            range_len: usize::min(elements_len.start, elements_len.end)..usize::max(elements_len.end, elements_len.start),
            counter: 0
        }).ok().map(#[inline(always)] |e| (e.0, e.1.count()));

        struct Builder {
            header_layout: Layout,
            element_layout: Layout,
            range_len: Range<usize>,
            counter: usize
        }

        impl BuilderDST for Builder {
            #[inline(always)]
            fn header_layout(&self) -> Layout {
                self.header_layout
            }

            #[inline(always)]
            fn element_layout(&self) -> Layout {
                self.element_layout
            }

            #[inline(always)]
            fn inserting_order(&self) -> InsertingOrder {
                InsertingOrder::Unspecified
            }

            #[inline(always)]
            fn elements_hint(&self) -> (usize, Option<usize>) {
                (self.range_len.start, Some(self.range_len.end))
            }

            #[inline(always)]
            fn write_header(&mut self, _: &mut [u8]) -> bool {
                true
            }

            #[inline(always)]
            fn write_element(&mut self, _: &mut [u8]) -> bool {
                if self.counter == self.range_len.end {
                    false
                } else {
                    self.counter += 1;
                    true
                }
            }

            #[inline(always)]
            fn finalizer(&mut self, _: &mut [u8]) -> bool {
                self.counter >= self.range_len.start
            }

            #[inline(always)]
            fn drop_header(&mut self, _: & mut [u8]) {
            }

            #[inline(always)]
            fn drop_element(&mut self, _: & mut [u8]) {
            }
        }
    }

    /// Attempts to allocate a slice with the specified element layout and range of elements.
    ///
    /// Allocation succeed with `Some(_)` if there is enough space to allocate the minimum number of elements specified by `elements_len`,
    /// if possible, it will attempt to allocate up to the maximum number in `element_len`.
    /// The first value is the allocation, and the second determines the state of number elements.
    fn try_alloc_remaining_slice_with_layout(&self, element_layout: Layout, elements_len: Range<usize>) -> Option<(Box<'_, [MaybeUninit<u8>]>, usize)> {
        return self.try_alloc_remaining_dst_with_builder(Builder {
            element_layout,
            range_len: usize::min(elements_len.start, elements_len.end)..usize::max(elements_len.end, elements_len.start),
            counter: 0
        }).ok().map(#[inline(always)] |e| (e.0, e.1.count()));

        struct Builder {
            element_layout: Layout,
            range_len: Range<usize>,
            counter: usize
        }

        impl BuilderDST for Builder {
            #[inline(always)]
            fn header_layout(&self) -> Layout {
                Layout::new::<()>()
            }

            #[inline(always)]
            fn element_layout(&self) -> Layout {
                self.element_layout
            }

            #[inline(always)]
            fn inserting_order(&self) -> InsertingOrder {
                InsertingOrder::Unspecified
            }

            #[inline(always)]
            fn elements_hint(&self) -> (usize, Option<usize>) {
                (self.range_len.start, Some(self.range_len.end))
            }

            #[inline(always)]
            fn write_header(&mut self, _: &mut [u8]) -> bool {
                true
            }

            #[inline(always)]
            fn write_element(&mut self, _: &mut [u8]) -> bool {
                if self.counter == self.range_len.end {
                    false
                } else {
                    self.counter += 1;
                    true
                }
            }

            #[inline(always)]
            fn finalizer(&mut self, _: &mut [u8]) -> bool {
                self.counter >= self.range_len.start
            }

            #[inline(always)]
            fn drop_header(&mut self, _: & mut [u8]) {
            }

            #[inline(always)]
            fn drop_element(&mut self, _: & mut [u8]) {
            }
        }
    }

    /// Attempts to allocate a slice with the specified element layout.
    ///
    /// It will attempt to allocate up to the maximum number in `element_len`.
    ///
    /// The first returned value is the allocation, and the second determines the state of number elements.
    fn alloc_remaining_slice_with_layout(&self, element_layout: Layout, maximum_len: usize) -> (Box<'_, [MaybeUninit<u8>]>, usize) {
        let result = self.try_alloc_remaining_dst_with_builder(Builder {
            element_layout,
            maximum_len,
            counter: 0
        }).expect("allocation failed: slice allocation should be supported (len 0).");
        return (result.0, result.1.count());

        struct Builder {
            element_layout: Layout,
            maximum_len: usize,
            counter: usize
        }

        impl BuilderDST for Builder {
            #[inline(always)]
            fn header_layout(&self) -> Layout {
                Layout::new::<()>()
            }

            #[inline(always)]
            fn element_layout(&self) -> Layout {
                self.element_layout
            }

            #[inline(always)]
            fn inserting_order(&self) -> InsertingOrder {
                InsertingOrder::Unspecified
            }

            #[inline(always)]
            fn elements_hint(&self) -> (usize, Option<usize>) {
                (0, Some(self.maximum_len))
            }

            #[inline(always)]
            fn write_header(&mut self, _: &mut [u8]) -> bool {
                true
            }

            #[inline(always)]
            fn write_element(&mut self, _: &mut [u8]) -> bool {
                if self.counter == self.maximum_len {
                    false
                } else {
                    self.counter += 1;
                    true
                }
            }

            #[inline(always)]
            fn finalizer(&mut self, _: &mut [u8]) -> bool {
                true
            }

            #[inline(always)]
            fn drop_header(&mut self, _: & mut [u8]) {
            }

            #[inline(always)]
            fn drop_element(&mut self, _: & mut [u8]) {
            }
        }
    }

    /// Allocates as many elements as possible from the given iterator into the current chunk's remaining space.
    ///
    /// The inserting order determines the order in which elements are layered in the slice.
    ///
    /// The iterator is returned if the chunk's remaining space got consumed before exhausting the iterator.
    fn alloc_remaining_slice_from_iter_with_order<T: IntoIterator>(&self, iter: T, inserting_order: InsertingOrder) -> (Box<'_, [T::Item]>, Option<T::IntoIter>) {
        struct Builder<T: Iterator> {
            iter: T,
            inserting_order: InsertingOrder,
        }

        impl<T: Iterator> BuilderDST for &mut Builder<T> {
            #[inline(always)]
            fn header_layout(&self) -> Layout {
                Layout::new::<()>()
            }

            #[inline(always)]
            fn element_layout(&self) -> Layout {
                Layout::new::<T::Item>()
            }

            #[inline(always)]
            fn inserting_order(&self) -> InsertingOrder {
                self.inserting_order
            }

            #[inline(always)]
            fn elements_hint(&self) -> (usize, Option<usize>) {
                (0, self.iter.size_hint().1)
            }

            #[inline(always)]
            fn write_header(&mut self, _: &mut [u8]) -> bool {
                true
            }

            #[inline(always)]
            fn write_element(&mut self, memory: &mut [u8]) -> bool {
                if let Some(e) = self.iter.next() {
                    unsafe { (memory.as_mut_ptr() as *mut T::Item).write(e) }
                    true
                } else {
                    false
                }
            }

            #[inline(always)]
            fn finalizer(&mut self, _: &mut [u8]) -> bool {
                true
            }

            #[inline(always)]
            fn drop_header(&mut self, _: &mut [u8]) {
            }

            #[inline(always)]
            fn drop_element(&mut self, memory: &mut [u8]) {
                unsafe { ptr::drop_in_place(memory.as_mut_ptr().cast::<T::Item>()) }
            }
        }

        let mut builder = Builder {
            iter: iter.into_iter(),
            inserting_order,
        };
        let result = self.try_alloc_remaining_dst_with_builder(&mut builder)
            .expect("iterator was supported to be of len 0, so this shouldn't have failed.");
        let (remaining, count) = match result.1 {
            WriteElementState::Started { count, completed: true } => (None, count),
            WriteElementState::Started { count, completed: false } => (Some(builder.iter), count),
            WriteElementState::NeverStarted => (Some(builder.iter), 0),
        };
        unsafe {
            let ptr = Box::into_non_null(result.0);
            (Box::from_non_null(ptr.cast::<T::Item>().cast_slice(count)), remaining)
        }
    }

    /// Allocates as many elements as possible from the given iterator into the current chunk's remaining space.
    ///
    /// The iterator is returned if the chunk's remaining space got consumed before exhausting the iterator.
    #[inline(always)]
    fn alloc_remaining_slice_from_iter<T: IntoIterator>(&self, iter: T) -> (Box<'_, [T::Item]>, Option<T::IntoIter>) {
        self.alloc_remaining_slice_from_iter_with_order(iter, InsertingOrder::Original)
    }

    /// Try to allocate an object in the arena.
    #[inline(always)]
    fn try_alloc<T>(&self, value: T) -> Result<Box<'_, T>, AllocError> {
        self.try_alloc_with(#[inline(always)] || value)
    }

    /// Try to allocate an object in the arena by cloning from a reference.
    #[inline(always)]
    fn try_alloc_from_clone<T: CloneToUninit + ?Sized>(&self, value: &T) -> Result<Box<'_, T>, AllocError> {
        let metadata = ptr::metadata(value);
        let layout = Layout::for_value(value);
        let allocation = self.try_alloc_layout(layout)?;
        let dst = NonNull::<T>::from_raw_parts(Box::into_non_null(allocation).cast::<()>(), metadata);
        unsafe { ptr::copy_nonoverlapping((value as *const T).cast::<u8>(), dst.cast::<u8>().as_ptr(), layout.size()); }
        Ok(unsafe { Box::from_non_null(dst) })
    }

    /// Try to allocate a slice in the arena.
    #[inline(always)]
    fn try_alloc_slice_clone<T: Clone>(&self, slice: &[T]) -> Result<Box<'_, [T]>, AllocError> {
        let allocation = self.try_alloc_layout(Layout::for_value(slice))?;
        let dst = Box::into_non_null(allocation).cast::<T>();
        for (i, v) in slice.iter().cloned().enumerate() {
            unsafe { dst.add(i).write(v); }
        }
        Ok(unsafe { Box::from_non_null(dst.cast_slice(slice.len())) })
    }

    /// Try to allocate a slice in the arena.
    #[inline(always)]
    fn try_alloc_slice_copy<T: Copy>(&self, slice: &[T]) -> Result<Box<'_, [T]>, AllocError> {
        let allocation = self.try_alloc_layout(Layout::for_value(slice))?;
        let dst = Box::into_non_null(allocation).cast::<T>();
        unsafe { ptr::copy_nonoverlapping(slice.as_ptr(), dst.as_ptr(), slice.len()); }
        Ok(unsafe { Box::from_non_null(dst.cast_slice(slice.len())) })
    }

    /// Try to allocate a slice of the specified element in the arena, filled with values of the specified function.
    #[inline(always)]
    fn try_alloc_slice_fill_clone<T: Clone>(&self, len: usize, value: &T) -> Result<Box<'_, [T]>, AllocError> {
        self.try_alloc_slice_fill_with(len, #[inline(always)] |_| value.clone())
    }

    /// Try to allocate a slice of the specified element in the arena, filled with values of the specified function.
    #[inline(always)]
    fn try_alloc_slice_fill_copy<T: Copy>(&self, len: usize, value: &T) -> Result<Box<'_, [T]>, AllocError> {
        self.try_alloc_slice_fill_with(len, #[inline(always)] |_| *value)
    }

    /// Try to allocate a slice of the specified element in the arena, filled with the default value
    #[inline(always)]
    fn try_alloc_slice_fill_default<T: Default>(&self, len: usize) -> Result<Box<'_, [T]>, AllocError> {
        self.try_alloc_slice_fill_with(len, #[inline(always)] |_| T::default())
    }

    /// Try to allocate a slice of the specified element in the arena, filled with values of the specified function.
    #[inline(always)]
    fn try_alloc_slice_fill_iter<T>(&self, iter: T) -> Result<Box<'_, [T::Item]>, AllocError>
        where
            T: IntoIterator,
            T::IntoIter: ExactSizeIterator {
        let mut iter = iter.into_iter();
        self.try_alloc_slice_fill_with(iter.len(), #[inline(always)] |_| iter.next().expect("Iterator supplied less elements that the promised by `len()`."))
    }

    /// Try to allocate a slice of the specified element in the arena, filled with values of the specified function.
    #[inline(always)]
    fn try_alloc_slice_fill_with<T>(&self, len: usize, mut f: impl FnMut(usize) -> T) -> Result<Box<'_, [T]>, AllocError> {
        let allocation = self.try_alloc_layout(Layout::array::<T>(len).map_err(#[inline(always)] |_| AllocError::InvalidLayout)?)?;
        let dst = Box::into_non_null(allocation).cast::<T>();
        for i in 0..len {
            unsafe { dst.add(i).write(f(i)); }
        }
        Ok(unsafe { Box::from_non_null(dst.cast_slice(len)) })
    }

    /// Try to allocate a slice in the arena.
    #[inline(always)]
    fn try_alloc_slice<T>(&self, len: usize) -> Result<Box<'_, [MaybeUninit<T>]>, AllocError> {
        let layout = Layout::array::<MaybeUninit<T>>(len).map_err(#[inline(always)] |_| AllocError::InvalidLayout)?;
        let allocation = self.try_alloc_layout(layout)?;
        let dst = Box::into_non_null(allocation);
        Ok(unsafe { Box::from_non_null(dst.cast::<MaybeUninit<T>>().cast_slice(len)) })
    }

    /// Attempts to allocate a slice of the specified minimum and maximum len using the current chunk's remaining space.
    ///
    /// When the returned option is `Some(_)`, the allocation's slice's len is guaranteed to be between the specified range, otherwise it returns `None`.
    #[inline(always)]
    fn try_alloc_remaining_slice<T>(&self, range_len: Range<usize>) -> Option<Box<'_, [MaybeUninit<T>]>> {
        struct Builder<T> {
            range_len: Range<usize>,
            counter: usize,
            phantom: PhantomData<T>,
        }

        impl<T> BuilderDST for Builder<T> {
            #[inline(always)]
            fn header_layout(&self) -> Layout {
                Layout::new::<()>()
            }

            #[inline(always)]
            fn element_layout(&self) -> Layout {
                Layout::new::<T>()
            }

            #[inline(always)]
            fn inserting_order(&self) -> InsertingOrder {
                InsertingOrder::Unspecified
            }

            #[inline(always)]
            fn elements_hint(&self) -> (usize, Option<usize>) {
                (self.range_len.start, Some(self.range_len.end))
            }

            #[inline(always)]
            fn write_header(&mut self, _: &mut [u8]) -> bool {
                true
            }

            #[inline(always)]
            fn write_element(&mut self, _: &mut [u8]) -> bool {
                if self.counter == self.range_len.end {
                    false
                } else {
                    self.counter += 1;
                    true
                }
            }

            #[inline(always)]
            fn finalizer(&mut self, _: &mut [u8]) -> bool {
                self.counter >= self.range_len.start
            }

            #[inline(always)]
            fn drop_header(&mut self, _: &mut [u8]) {
            }

            #[inline(always)]
            fn drop_element(&mut self, _: &mut [u8]) {
            }
        }

        let element_layout = Layout::new::<T>();
        if element_layout.size() > 0 {
            let (slice, write_element_state) = self.try_alloc_remaining_dst_with_builder(Builder::<T> {
                range_len: usize::min(range_len.start, range_len.end)..usize::max(range_len.start, range_len.end),
                counter: 0,
                phantom: PhantomData
            }).ok()?;
            let dst = Box::into_non_null(slice);
            let len = write_element_state.count();
            Some(unsafe { Box::from_non_null(dst.cast::<MaybeUninit<T>>().cast_slice(len)) })
        } else {
            let max = usize::max(range_len.start, range_len.end);
            Some(unsafe { Box::from_non_null(NonNull::dangling().cast_slice(max)) })
        }
    }

    /// Attempts to allocate a slice of the specified len using the current chunk remaining space.
    ///
    /// If there is not enough space, return a smaller slice with the remaining space.
    ///
    /// Note that the slice may be of length 0 if there is not enough space to store any element.
    #[inline(always)]
    fn alloc_slice_from_remaining<T>(&self, maximum_len: usize) -> Box<'_, [MaybeUninit<T>]> {
        struct Builder<T> {
            maximum_len: usize,
            counter: usize,
            phantom: PhantomData<T>,
        }

        impl<T> BuilderDST for Builder<T> {
            #[inline(always)]
            fn header_layout(&self) -> Layout {
                Layout::new::<()>()
            }

            #[inline(always)]
            fn element_layout(&self) -> Layout {
                Layout::new::<T>()
            }

            #[inline(always)]
            fn inserting_order(&self) -> InsertingOrder {
                InsertingOrder::Unspecified
            }

            #[inline(always)]
            fn elements_hint(&self) -> (usize, Option<usize>) {
                (0, Some(self.maximum_len))
            }

            #[inline(always)]
            fn write_header(&mut self, _: &mut [u8]) -> bool {
                true
            }

            #[inline(always)]
            fn write_element(&mut self, _: &mut [u8]) -> bool {
                if self.counter == self.maximum_len {
                    false
                } else {
                    self.counter += 1;
                    true
                }
            }

            #[inline(always)]
            fn finalizer(&mut self, _: &mut [u8]) -> bool {
                true
            }

            #[inline(always)]
            fn drop_header(&mut self, _: &mut [u8]) {
            }

            #[inline(always)]
            fn drop_element(&mut self, _: &mut [u8]) {
            }
        }

        let element_layout = Layout::new::<T>();
        if element_layout.size() > 0 {
            let (slice, write_element_state) = self.try_alloc_remaining_dst_with_builder(Builder::<T> {
                maximum_len: maximum_len,
                counter: 0,
                phantom: PhantomData
            }).expect("allocation failed: slice allocation should be supported (len 0).");
            let dst = Box::into_non_null(slice);
            let len = write_element_state.count();
            unsafe { Box::from_non_null(dst.cast::<MaybeUninit<T>>().cast_slice(len)) }
        } else {
            unsafe { Box::from_non_null(NonNull::dangling().cast_slice(maximum_len)) }
        }
    }

    /// Try to allocate an str in the arena.
    #[inline(always)]
    fn try_alloc_str(&self, str: &str) -> Result<Box<'_, str>, AllocError> {
        let str = self.try_alloc_slice_copy(str.as_bytes())?;
        // This is safe because the bytes were acquired from a `str` which we can assume it was already validated.
        Ok(unsafe { Box::from_raw(str::from_utf8_unchecked_mut(&mut *Box::into_raw(str))) })
    }

    /// Try to allocate an uninit object in the arena.
    #[inline(always)]
    fn try_alloc_uninit<T>(&self) -> Result<Box<'_, MaybeUninit<T>>, AllocError> {
        let dst = self.try_alloc_layout(Layout::new::<MaybeUninit<T>>())?;
        Ok(unsafe { Box::from_non_null(Box::into_non_null(dst).cast::<MaybeUninit<T>>()) })
    }

    /// Try to allocate an object in the arena.
    #[inline(always)]
    fn try_alloc_with<T>(&self, f: impl FnOnce() -> T) -> Result<Box<'_, T>, AllocError> {
        let allocation = self.try_alloc_layout(Layout::new::<T>())?;
        let ptr = Box::into_non_null(allocation).cast::<T>();
        unsafe { inner_writer(ptr.as_ptr(), f); }
        return Ok(unsafe { Box::from_non_null(ptr.cast::<T>()) });

        #[inline(always)]
        unsafe fn inner_writer<T>(ptr: *mut T, f: impl FnOnce() -> T) {
            // Place separately to ensure LLVM realizes it can write directly into the heap without stack allocation.
            unsafe { ptr::write(ptr, f()); }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::mem::MaybeUninit;

    use crate::{AllocError, Box, BuilderDST, LocalArena, SharedArena, arena_test::arena_test_};

    fn identity(a: LocalArena) -> SimpleArena {
        SimpleArena(a)
    }

    struct SimpleArena(LocalArena);

    impl Arena for SimpleArena {
        fn try_alloc_layout(&self, layout: Layout) -> Result<Box<'_, [MaybeUninit<u8>]>, AllocError> {
            self.0.try_alloc_layout(layout)
        }

        fn try_alloc_remaining_dst_with_builder<B: BuilderDST>(&self, builder: B) -> Result<(Box<'_, [MaybeUninit<u8>]>, WriteElementState), CancellationError> {
            self.0.try_alloc_remaining_dst_with_builder(builder)
        }

        fn reset(&mut self) {
            self.0.reset();
        }

        fn allocation_info(&self) -> crate::AllocationInfo {
            self.0.allocation_info()
        }

        fn remaining_chunk_capacity(&self) -> usize {
            self.0.remaining_chunk_capacity()
        }
    }

    arena_test_!(identity);
}