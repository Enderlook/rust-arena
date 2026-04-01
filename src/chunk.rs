use std::{alloc::{Layout, alloc, dealloc}, mem::{ManuallyDrop, MaybeUninit}, num::NonZero, ops::Range, ptr::{self, NonNull}, slice, usize};

use crate::{AllocError, Box, BuilderDST, CancellationError, InsertingOrder, WriteElementState};

use crate::compatibility::*;

#[repr(C)]
pub(crate) struct ChunkHeader {
    /// Current pointer of the bump, which moves downwards.
    pub(crate) current_bump_ptr: NonNull<u8>,
    pub(crate) prev: Option<ChunkPtr>,
    pub(crate) len: usize,
}

impl Drop for ChunkHeader {
    fn drop(&mut self) {
        if let Some(e) = &mut self.prev {
            let ptr = e.as_ptr();
            unsafe {
                let layout = (*e).get_layout();
                ptr::drop_in_place(ptr);
                dealloc(ptr.cast::<u8>(), layout)
            }
        }
    }
}

#[repr(C)]
pub(crate) struct Chunk {
    pub(crate) header: ChunkHeader,
    pub(crate) storage: [u8],
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub(crate) struct ChunkPtr(NonNull<()>);

impl ChunkPtr {
    #[inline(always)]
    pub(crate) fn new(ptr: NonNull<Chunk>) -> Self {
        Self(ptr.cast())
    }

    #[inline(always)]
    pub(crate) fn as_non_null(self) -> NonNull<Chunk> {
        unsafe { nonnull_from_raw_parts!(self.0, self.0.cast::<ChunkHeader>().as_ref().len) }
    }

    #[inline(always)]
    pub(crate) fn as_ptr(self) -> *mut Chunk {
        unsafe { ptr_from_raw_parts_mut!(self.0.as_ptr(), self.0.cast::<ChunkHeader>().as_ref().len) }
    }

    #[inline(always)]
    pub(crate) unsafe fn as_mut(&mut self) -> &mut Chunk {
        unsafe { &mut *self.as_ptr() }
    }

    #[inline(always)]
    pub(crate) unsafe fn as_ref(&self) -> &Chunk {
        unsafe { & *self.as_ptr() }
    }

    #[inline(always)]
    pub(crate) fn are_equal(&self, other: ChunkPtr) -> bool {
        ptr::eq(self.0.as_ptr(), other.0.as_ptr())
    }

    #[inline(always)]
    pub(crate) fn get_layout(&self) -> Layout {
        // return unsafe { Layout::for_value_raw(self.as_ptr()) };
        unsafe { Layout::new::<ChunkHeader>()
            .extend(Layout::array::<u8>(self.0.cast::<ChunkHeader>().as_ref().len ).map_err(#[inline(always)] |_| AllocError::InvalidLayout).unwrap_unchecked())
            .map_err(#[inline(always)] |_| AllocError::InvalidLayout).unwrap_unchecked() }
            .0
            .pad_to_align()
    }
}

impl Chunk {
    #[inline]
    pub(crate) fn create_chunk(size: usize) -> Result<ChunkPtr, AllocError> {
        let (layout, extend) = Layout::new::<ChunkHeader>()
            .extend(Layout::array::<u8>(size).map_err(#[inline(always)] |_| AllocError::InvalidLayout)?)
            .map_err(#[inline(always)] |_| AllocError::InvalidLayout)?;
        let layout = layout.pad_to_align();

        let ptr = unsafe { alloc(layout) };

        if ptr.is_null() {
            return Err(AllocError::OutOfMemory);
        }

        let mut chunk = unsafe { NonNull::<Chunk>::new_unchecked(ptr_from_raw_parts_mut!(ptr as *mut (), size)) };
        let chunk_ref = unsafe { chunk.as_mut() };
        chunk_ref.header.current_bump_ptr = unsafe { NonNull::new_unchecked(ptr.add(extend).add(size)) };
        chunk_ref.header.prev = None;
        chunk_ref.header.len = size;
        Ok(ChunkPtr::new(chunk))
    }

    #[inline(always)]
    pub(crate) fn can_allocate(&self, layout: Layout) -> bool {
        let size = layout.size();

        let start_ptr = self.storage.as_ptr().addr();
        let current_ptr = self.header.current_bump_ptr.as_ptr().addr();

        let align = layout.align();
        debug_assert!(align > 0);
        debug_assert!(align.is_power_of_two());
        // Round pointer down to alignment.
        // The idea is to make alignment of allocations independent of the chunk alignment,
        // for that reason with align with the actual address rather than an offset.
        let aligned_ptr = current_ptr & !(align - 1);

        if aligned_ptr < start_ptr {
            return false;
        }

        let remaining = aligned_ptr - start_ptr;

        // Round size up to alignment (safe because `Layout` guarantees no overflow).
        let aligned_size = match size.checked_add(align - 1) {
            Some(v) => v & !(align - 1),
            None => return false,
        };

        aligned_size <= remaining
    }

    #[inline(always)]
    pub(crate) fn try_alloc(&mut self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        // We don't handle ZSTs explicitly to avoid additional cost in checks for a very rare case,
        // if this is one of those cases, this still works, but may waste memory on unnecessary alignment.

        let align = layout.align();
        debug_assert!(align > 0);
        debug_assert!(align.is_power_of_two());

        let start_ptr = self.storage.as_ptr().addr();
        let current_ptr = self.header.current_bump_ptr.as_ptr().addr();
        debug_assert!(current_ptr >= start_ptr);

        // Round pointer down to alignment.
        // The idea is to make alignment of allocations independent of the chunk alignment,
        // for that reason with align with the actual address rather than an offset.
        let aligned_ptr = current_ptr & !(align - 1);

        // Round size up to alignment.
        let size = layout.size();
        // Layout ensures that align is greater than 0 and size rounded up to align is never greater than `isize::MAX`,
        // so this can't overflow.
        let aligned_size = (size + (align - 1)) & !(align - 1);
        if aligned_ptr < start_ptr || aligned_size > (aligned_ptr - start_ptr) {
            return Err(AllocError::OutOfMemory);
        }

        // Can't overflow since check above ensures that `aligned_size <= aligned_ptr - start_ptr`.
        let new_ptr = aligned_ptr - aligned_size;
        if new_ptr < start_ptr {
            return Err(AllocError::OutOfMemory);
        }

        let new_ptr = self.header.current_bump_ptr.with_addr(unsafe { NonZero::new_unchecked(new_ptr) });
        self.header.current_bump_ptr = new_ptr;
        Ok(new_ptr)
    }

    #[inline(always)]
    pub(crate) fn alloc_remaining_slice_with_layout(&mut self, element_layout: Layout, maximum_len: usize) -> (Box<'_, [MaybeUninit<u8>]>, usize) {
        let start_ptr = self.storage.as_ptr().addr();
        let current_ptr = self.header.current_bump_ptr.as_ptr().addr();
        debug_assert!(current_ptr >= start_ptr);

        let align = element_layout.align();
        debug_assert!(align > 0);
        debug_assert!(align.is_power_of_two());
        // Round pointer down to alignment.
        let aligned_ptr = current_ptr & !(align - 1);
        if aligned_ptr < start_ptr {
            return (unsafe { Box::from_non_null(NonNull::dangling().cast_slice_(0)) }, 0);
        }

        let remaining_bytes = aligned_ptr - start_ptr;

        let element_size = element_layout.size();
        // In the case of having a ZST, the `element_size` would be 0, so we require to handle that.
        let max_elements_fit = remaining_bytes.checked_div(element_size).unwrap_or(maximum_len);
        let element_count = usize::min(max_elements_fit, maximum_len);
        let total_size = element_count * element_size;

        let new_ptr = aligned_ptr - total_size;
        let new_ptr = self.storage.as_mut_ptr().with_addr(new_ptr);
        self.header.current_bump_ptr = unsafe { NonNull::new_unchecked(new_ptr) };

        (unsafe { Box::from_raw(new_ptr.cast::<MaybeUninit<u8>>().cast_slice_(total_size)) }, element_count)
    }

    #[inline(always)]
    pub(crate) fn try_alloc_remaining_slice_with_layout(&mut self, element_layout: Layout, range_len: Range<usize>) -> Option<(Box<'_, [MaybeUninit<u8>]>, usize)> {
        let start_ptr = self.storage.as_ptr().addr();
        let current_ptr = self.header.current_bump_ptr.as_ptr().addr();
        debug_assert!(current_ptr >= start_ptr);

        let align = element_layout.align();
        debug_assert!(align > 0);
        debug_assert!(align.is_power_of_two());
        // Round pointer down to alignment.
        let aligned_ptr = current_ptr & !(align - 1);
        if aligned_ptr < start_ptr {
            return if range_len.start == 0 {
                Some((unsafe { Box::from_non_null(NonNull::dangling().cast_slice_(0)) }, 0))
            } else {
                None
            };
        }

        let remaining_bytes = aligned_ptr - start_ptr;

        let element_size = element_layout.size();
        // In the case of having a ZST, the `element_size` would be 0, so we require to handle that.
        let max_elements_fit = remaining_bytes.checked_div(element_size).unwrap_or(range_len.end);
        let element_count = usize::min(max_elements_fit, range_len.end);
        if element_count < range_len.start {
            return None;
        }
        let total_size = element_count * element_size;

        let new_ptr = aligned_ptr - total_size;
        let new_ptr = self.storage.as_mut_ptr().with_addr(new_ptr);
        self.header.current_bump_ptr = unsafe { NonNull::new_unchecked(new_ptr) };

        Some((unsafe { Box::from_raw(new_ptr.cast::<MaybeUninit<u8>>().cast_slice_(total_size)) }, element_count))
    }

    #[inline(always)]
    pub(crate) fn try_alloc_remaining_dst_with_layout(&mut self, header_layout: Layout, element_layout: Layout, elements_len: Range<usize>) -> Option<(Box<'_, [MaybeUninit<u8>]>, usize)> {
        let start_ptr = self.storage.as_ptr().addr();
        let current_ptr = self.header.current_bump_ptr.as_ptr().addr();
        debug_assert!(current_ptr >= start_ptr);

        // Calculate the alignment of the DST.
        // This works because the alignment of `T` and `[T; N]` is the same.
        let (align, header_offset) = match element_layout
            .repeat_(1)
            .ok()
            .map(#[inline(always)] |e| header_layout.extend(e.0).ok())
            .flatten() {
            Some((layout, offset)) => (layout.pad_to_align().align(), offset),
            None => return None,
        };
        debug_assert!(align > 0);
        debug_assert!(align.is_power_of_two());

        // Round pointer down to alignment.
        let aligned_ptr = current_ptr & !(align - 1);

        // Reserve space for the header.
        let header_size = header_layout.size();
        let new_start_ptr = match start_ptr.checked_add(header_offset) {
            Some(new_start_ptr) if aligned_ptr < new_start_ptr => return None,
            Some(new_start_ptr) => new_start_ptr,
            None => return None,
        };

        let remaining_space = aligned_ptr - new_start_ptr;
        let element_size = element_layout.size();
        // In the case of having a ZST, the `element_size` would be 0, so we require to handle that.
        let max_fit = remaining_space.checked_div(element_size).unwrap_or(usize::MAX);

        debug_assert_eq!(Some(align), element_layout.repeat_(max_fit).map(|e| header_layout.extend(e.0)).ok().map(|e| e.ok()).flatten().map(|e| e.0.pad_to_align().align()));

        if max_fit < elements_len.start {
            return None;
        }

        let element_count = usize::min(elements_len.end, max_fit);

        debug_assert!(element_count.checked_mul(element_size).map(|e| header_offset.checked_add(e)).is_some(), "Can't overflow since we previously did some calculations that checked it.");
        let total_size = header_size + element_count * element_size;

        let new_ptr = aligned_ptr - total_size;
        let new_ptr = self.storage.as_mut_ptr().with_addr(new_ptr);
        self.header.current_bump_ptr = unsafe { NonNull::new_unchecked(new_ptr) };

        Some((unsafe { Box::from_raw(new_ptr.cast::<MaybeUninit<u8>>().cast_slice_(total_size)) }, element_count))
    }

    #[inline(always)]
    pub(crate) fn try_alloc_remaining_dst_with_builder<'a, 'b>(&'a mut self, info: &'a DSTInfo) -> Result<AllocDSTBuilder<'a>, ()> {
        let storage_ptr = self.storage.as_mut_ptr();
        let start_ptr = storage_ptr.addr();
        let current_ptr = self.header.current_bump_ptr.as_ptr().addr();
        debug_assert!(current_ptr >= start_ptr);

        // Round pointer down to alignment.
        let aligned_ptr = current_ptr & !(info.align - 1);

        // The min_layout already includes the header, offset and the size required to store the minimum amount of elements
        // so we check if we have enough space for at least that.
        match start_ptr.checked_add(info.min_layout.size()) {
            Some(new_start_ptr) if aligned_ptr < new_start_ptr => return slow_path_1(self, info),
            Some(_) => {},
            None => return Err(()),
        };

        return Ok(AllocDSTBuilder::Normal { chunk: self, info, aligned_ptr });

        #[cold]
        fn slow_path_1<'a, 'b>(this: &'a mut Chunk, info: &'b DSTInfo) -> Result<AllocDSTBuilder<'a>, ()> {
            if info.header_offset == 0 {
                debug_assert_eq!(info.header_layout.size(), 0);
                if info.min_element == 0 {
                    return Ok(AllocDSTBuilder::Rare { chunk: this, count: true });
                } else if info.element_layout.size() == 0 {
                    return Ok(AllocDSTBuilder::Rare { chunk: this, count: false });
                }
            }
            Err(())
        }
    }

    pub(crate) fn alloc_remaining_slice_from_iter_with_order<T: Iterator>(&mut self, mut iter: T, inserting_order: InsertingOrder) -> (Box<'_, [T::Item]>, Option<T>) {
        type Item<I> = <I as Iterator>::Item;

        struct Guard<T> {
            current: *mut T,
            written: usize,
        }

        impl<T> Drop for Guard<T> {
            fn drop(&mut self) {
                unsafe {
                    for i in 0..self.written {
                        std::ptr::drop_in_place(self.current.add(i));
                    }
                }
            }
        }

        // An array of [T; N] has the size of T * N and the same alignment of T.
        let layout = Layout::new::<Item<T>>();
        let size = layout.size();

        let storage_ptr = self.storage.as_mut_ptr();
        let start_ptr = storage_ptr.addr();
        let current_ptr = self.header.current_bump_ptr.as_ptr().addr();
        debug_assert!(current_ptr >= start_ptr);

        let align = layout.align();
        // Layout ensures that align is greater than 0.
        let aligned_ptr = current_ptr & !(align - 1);
        if aligned_ptr < start_ptr {
            return (unsafe { Box::from_non_null(NonNull::dangling().cast_slice_(0)) }, None);
        }

        let max_fit = (aligned_ptr - start_ptr) / size;
        if max_fit == 0 {
            return (unsafe { Box::from_non_null(NonNull::dangling().cast_slice_(0)) }, Some(iter));
        }

        // Write from the end backward so simplify shrinking the allocation.
        let mut guard = Guard {
            current: storage_ptr.with_addr(aligned_ptr).cast::<Item<T>>(),
            written: 0
        };

        let mut to_completion = false;
        while guard.written < max_fit {
            match iter.next() {
                Some(item) => {
                    unsafe {
                        guard.current = guard.current.sub(1);
                        guard.current.write(item);
                    }
                    guard.written += 1;
                },
                None => {
                    to_completion = true;
                    break
                },
            }
        }

        // Prevent dropping.
        let guard = ManuallyDrop::new(guard);

        let remaining = if to_completion { None } else { Some(iter) };

        if guard.written == 0 {
            return (unsafe { Box::from_non_null(NonNull::dangling().cast_slice_(0)) }, remaining);
        }

        // Move bump pointer only for written elements.
        debug_assert_eq!(aligned_ptr - (guard.written * size), guard.current.addr());
        let final_ptr = unsafe { NonNull::new_unchecked(guard.current.cast::<u8>()) };
        self.header.current_bump_ptr = final_ptr;

        let mut slice = unsafe { Box::from_non_null(final_ptr.cast::<T::Item>().cast_slice_(guard.written)) };
        // By default we must to reverse in order to get the original order since we use a downward bump allocator.
        if matches!(inserting_order, InsertingOrder::Original) {
            slice.reverse();
        }
        (slice, remaining)
    }
}

pub(crate) enum AllocDSTBuilder<'a> {
    Normal {
        chunk: &'a mut Chunk,
        info: &'a DSTInfo,
        aligned_ptr: usize,
    },
    Rare {
        chunk: &'a mut Chunk,
        count: bool
    },
}

impl<'a> AllocDSTBuilder<'a> {
    pub fn finish<'b>(self, builder: &mut impl BuilderDST) -> Result<(Box<'b, [MaybeUninit<u8>]>, WriteElementState), CancellationError> {
        let (chunk, builder, info, aligned_ptr) = match self {
            Self::Normal { chunk, info, aligned_ptr } => (chunk, builder, info, aligned_ptr),
            Self::Rare { count, .. } => return slow_path_1(builder, count),
        };

        let storage_ptr = chunk.storage.as_mut_ptr();
        let start_ptr = storage_ptr.addr();
        let current_ptr = chunk.header.current_bump_ptr.as_ptr().addr();
        debug_assert!(current_ptr >= start_ptr);

        let header_offset = info.header_offset;
        let element_layout = info.element_layout;

        // Reserve space for the header.
        debug_assert!(start_ptr.checked_add(header_offset).is_some(), "Can't overflow because the addition above didn't, and header_offset is equal or smaller than info.min_layout.size().");
        let new_start_ptr = unsafe { start_ptr.unchecked_add(header_offset) };

        // Write from the end backward so simplify shrinking the allocation.
        let mut guard = Guard {
            current: storage_ptr.with_addr(aligned_ptr),
            written: 0,
            reverse: false,
            builder,
            element_size: element_layout.size(),
            header_size: info.header_layout.size(),
            header_offset,
            header_written: false,
        };

        let mut write_element_state = WriteElementState::NeverStarted;

        // Check if we can write at least one element.
        if guard.current.addr() > new_start_ptr {
            write_element_state = WriteElementState::Started { count: 0, completed: false };
            // Note that for ZST tail element this loop can be very long,
            // but we expect the builder implementation to stop at some point.
            while guard.current.addr() > new_start_ptr {
                // For ZST tail elements this will always give the same address,
                // but it's not a problem since you can't write in a ZST.
                let current = unsafe { guard.current.sub(guard.element_size) };
                let element_memory = unsafe { slice::from_raw_parts_mut(current, guard.element_size) };
                if !guard.builder.write_element(element_memory) {
                    write_element_state = WriteElementState::Started { count: guard.written, completed: true };
                    break;
                }
                guard.current = current;
                guard.written += 1;
            }
        } else if element_layout.size() == 0 {
            write_element_state = slow_path_2(&mut guard);
        }

        let final_ptr = unsafe { NonNull::new_unchecked(guard.current) };
        let slice = final_ptr.cast_slice_(guard.written * guard.element_size);
        // By default we must to reverse in order to get the original order since we use a downward bump allocator.
        if matches!(guard.builder.inserting_order(), InsertingOrder::Original) {
            let len = guard.written;
            let size = element_layout.size();
            if size > 0  {
                let ptr = slice.as_mut_ptr_();
                let mut a = ptr;
                for i in 0..(len / 2) {
                    unsafe {
                        let b = ptr.add((len - 1 - i) * size);
                        ptr::swap_nonoverlapping(a, b, size);
                        a = a.add(size);
                    }
                }
            }
            guard.reverse = true;
        }

        let header_start = unsafe { guard.current.sub(header_offset) };
        let header_memory = unsafe { slice::from_raw_parts_mut(header_start, guard.header_size) };
        if guard.builder.write_header(header_memory) {
            guard.header_written = true;
        } else {
            drop(guard);
            return Err(CancellationError::CancelledByHeader(write_element_state));
        }

        debug_assert!(guard.written.checked_mul(guard.element_size).map(|e| header_offset.checked_add(e)).is_some(), "Can't overflow since we previously did some calculations that checked it.");
        let memory_len = header_offset + guard.written * guard.element_size;
        let memory = unsafe { slice::from_raw_parts_mut(header_start, memory_len) };
        if !guard.builder.finalizer(memory) {
            drop(guard);
            return Err(CancellationError::CancelledByFinalizer(write_element_state));
        }

        let _ = ManuallyDrop::new(guard);

        chunk.header.current_bump_ptr = unsafe { NonNull::new_unchecked(header_start) };

        let slice = unsafe { Box::from_non_null(NonNull::new_unchecked(ptr_from_raw_parts_mut!(header_start, memory_len))) };

        return Ok((slice, write_element_state));

        struct Guard<'b, B: BuilderDST> {
            current: *mut u8,
            written: usize,
            reverse: bool,
            element_size: usize,
            header_size: usize,
            header_offset: usize,
            builder: &'b mut B,
            header_written: bool,
        }

        impl<'b, B: BuilderDST> Drop for Guard<'b, B> {
            #[cold]
            fn drop(&mut self) {
                let mut current = self.current;
                // Ensure elements are dropped in the same order they were allocated.
                if self.reverse {
                    // `current` points to the last allocated element,
                    // which since the content was previously reversed,
                    // it contains the actual first allocated element.
                    for _ in 0..self.written {
                        self.builder.drop_element(unsafe { slice::from_raw_parts_mut(current, self.element_size) });
                        current = unsafe { current.add(self.element_size) };
                    }
                } else {
                    // `current` points to the last allocated element,
                    // so we must alter the pointer to point to the first allocated element.
                    current = unsafe { self.current.add(self.element_size * self.written) };
                    for _ in 0..self.written {
                        current = unsafe { current.sub(self.element_size) };
                        self.builder.drop_element(unsafe { slice::from_raw_parts_mut(current, self.element_size) });
                    }
                }

                if self.header_written {
                    let memory = unsafe { slice::from_raw_parts_mut(self.current.sub(self.header_offset), self.header_size) };
                    self.builder.drop_header(memory);
                }
            }
        }

        #[cold]
        fn slow_path_1<'a>(builder: &mut impl BuilderDST, count: bool) -> Result<(Box<'a, [MaybeUninit<u8>]>, WriteElementState), CancellationError> {
            if count {
                if builder.write_header(&mut []) {
                    if builder.finalizer(&mut []) {
                        Ok((unsafe { Box::from_non_null(nonnull_from_raw_parts!(NonNull::<u8>::dangling(), 0)) }, WriteElementState::NeverStarted))
                    } else {
                        Err(CancellationError::CancelledByFinalizer(WriteElementState::NeverStarted))
                    }
                } else {
                    Err(CancellationError::CancelledByHeader(WriteElementState::NeverStarted))
                }
            } else {
                if builder.write_header(&mut []) {
                    while builder.write_element(&mut []) {}
                    if builder.finalizer(&mut []) {
                        Ok((unsafe { Box::from_non_null(nonnull_from_raw_parts!(NonNull::<u8>::dangling(), 0)) }, WriteElementState::NeverStarted))
                    } else {
                        Err(CancellationError::CancelledByFinalizer(WriteElementState::NeverStarted))
                    }
                } else {
                    Err(CancellationError::CancelledByHeader(WriteElementState::NeverStarted))
                }
            }
        }

        #[cold]
        fn slow_path_2<'a, B: BuilderDST>(guard: &mut Guard<'a, B>) -> WriteElementState {
            let mut count = 0;
            while guard.builder.write_element(&mut []) {
                count += 1;
            }
            WriteElementState::Started { count, completed: true }
        }
    }
}

pub(crate) struct DSTInfo {
    pub(crate) header_layout: Layout,
    pub(crate) element_layout: Layout,
    pub(crate) min_layout: Layout,
    pub(crate) min_element: usize,
    pub(crate) header_offset: usize,
    pub(crate) align: usize
}

impl DSTInfo {
    #[inline(always)]
    pub(crate) fn new(builder: &impl BuilderDST) -> Option<DSTInfo> {
        let header = builder.header_layout();
        let element = builder.element_layout();
        let (min_element, _) = builder.elements_hint();
        let (mut min_layout, header_offset, align) = if min_element > 0 {
            let (min_layout, header_offset) = header.extend(element.repeat_(min_element).ok()?.0).ok()?;
            // Safe because it already could produce a bigger layout.
            let align = unsafe { header.extend(element.repeat_(1).unwrap_unchecked().0).unwrap_unchecked().0.pad_to_align().align() };
            (min_layout, header_offset, align)
        } else {
            let align = header.extend(element.repeat_(1).ok()?.0).ok()?.0.pad_to_align().align();
            // Safe because it already could produce a bigger layout.
            let (min_layout, header_offset) = unsafe { header.extend(element.repeat_(0).unwrap_unchecked().0).unwrap_unchecked() };
            (min_layout, header_offset, align)
        };
        min_layout = min_layout.pad_to_align();
        debug_assert!(align > 0);
        debug_assert!(align.is_power_of_two());
        Some(DSTInfo { header_layout: header, element_layout: element, min_layout, min_element, header_offset, align })
    }
}
