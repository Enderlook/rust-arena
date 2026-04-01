use std::{alloc::Layout, iter, marker::PhantomData, mem::{self, ManuallyDrop, MaybeUninit}, ptr::{self, NonNull}};

use crate::{AllocError, Arena, Box, LocalArena};

use crate::compatibility::*;

/// Represent an unrolled linked list.
///
/// That is, a list whose internal memory is split into linked segments.
///
/// The memory allocations of this structure are from an arena that supports arbitrary types.
pub struct UnrolledList<'a, T, A: Arena = LocalArena> {
    content: Option<Content<T>>,
    arena: &'a A,
}

struct Content<T> {
    /// Number of used elements.
    len: usize,
    /// Amount of elements used in the last node.
    last_node_used: usize,
    /// Pointer to the first node of the list.
    first: NodePtr<T>,
    /// Pointer to the last used node of the list.
    ///
    /// There may be more unused nodes if the list got many elements removed.
    last: NodePtr<T>,
}

#[repr(C)]
struct Node<T> {
    header: NodeHeader<T>,
    array: [MaybeUninit<T>],
}

#[repr(C)]
struct NodeHeader<T> {
    capacity: usize,
    prev: Option<NodePtr<T>>,
    next: Option<NodePtr<T>>,
}

/// The idea of this struct is to avoid fat pointers by storing the len of `Node<T>` in its header.
#[repr(transparent)]
struct NodePtr<T>(NonNull<()>, PhantomData<T>);

impl<T> NodePtr<T> {
    #[inline(always)]
    fn header(self) -> *mut NodeHeader<T> {
        unsafe { self.0.cast::<NodeHeader<T>>().as_mut() }
    }

    #[inline(always)]
    fn array(mut self) -> *mut [MaybeUninit<T>] {
        unsafe { ptr::slice_from_raw_parts_mut((*self.as_ptr()).array.as_mut_ptr(), (*self.header()).capacity) }
    }

    #[inline(always)]
    unsafe fn as_ptr(&mut self) -> *mut Node<T> {
        unsafe { ptr_from_raw_parts_mut!(self.0.as_ptr(), self.0.cast::<NodeHeader<T>>().as_ref().capacity) }
    }
}

impl<T> PartialEq for NodePtr<T> {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        ptr::eq(self.0.as_ptr(), other.0.as_ptr())
    }
}

impl<T> Clone for NodePtr<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone(), self.1.clone())
    }
}

impl<T> Copy for NodePtr<T> {}

impl<'a, T, A: Arena> Drop for UnrolledList<'a, T, A> {
    fn drop(&mut self) {
        // Don't need to free the nodes themselves because they are arena-allocated and are never individually deallocated.
        // So we only deallocate their content.
        if let Some(content) = &mut self.content {
            let mut len = content.len;
            let mut node = &mut content.first;
            loop {
                let array = unsafe { &mut *node.array() };
                if len > array.len() {
                    len -= array.len();
                    for e in array.iter_mut() {
                        unsafe { e.assume_init_drop(); }
                    }
                    let header = unsafe { &mut *node.header() };
                    debug_assert!(header.next.is_some(), "Next is none despite there is remaining len");
                    node = unsafe { header.next.as_mut().unwrap_unchecked() };
                } else {
                    for e in unsafe { array.get_unchecked_mut(0..len) } {
                        unsafe { e.assume_init_drop(); }
                    }
                    break;
                }
            }
        }
    }
}

impl<'a, T, A: Arena> UnrolledList<'a, T, A> {
    /// Creates a new instance.
    #[inline(always)]
    pub fn new(arena: &'a A) -> Self {
        Self {
            content: None,
            arena,
        }
    }

    /// Gets an iterator over the references of this collection.
    #[inline]
    pub fn iter(&self) -> IteratorRef<'a, T> {
        if let Some(content) = &self.content {
            IteratorRef {
                remaining: content.len,
                index: 0,
                chunk: Some(content.first),
                phantom: PhantomData
            }
        } else {
            IteratorRef { remaining: 0, index: 0, chunk: None, phantom: PhantomData }
        }
    }

    /// Gets an iterator over the mutable references of this collection.
    #[inline]
    pub fn iter_mut(&mut self) -> IteratorMut<'a, T> {
        if let Some(content) = &self.content {
            IteratorMut {
                remaining: content.len,
                index: 0,
                chunk: Some(content.first),
                phantom: PhantomData
            }
        } else {
            IteratorMut { remaining: 0, index: 0, chunk: None, phantom: PhantomData }
        }
    }

    /// Adds a new content at the end of the list.
    #[inline]
    pub fn add(&mut self, value: T) -> Result<(), AllocError> {
        if let Some(mut content) = self.content.as_mut() {
            if unsafe { &mut *content.last.array() }.len() <= content.last_node_used {
                Self::slow_add(self.arena, &mut content)?;
            }
            *unsafe { &mut *content.last.array().get_unchecked_mut_(content.last_node_used) } = MaybeUninit::new(value);
            content.last_node_used += 1;
            content.len += 1;
        } else {
            self.first_add(value)?;
        }
        Ok(())
    }

    /// Get the reference of an element at the specified index.
    #[inline]
    pub fn get(&self, mut index: usize) -> Option<&T> {
        if let Some(content) = &self.content {
            if index < content.len {
                let mut node = &content.first;
                loop {
                    debug_assert!(*node != content.last || index < content.last_node_used, "Using an uninitialized node despite previous len check.");
                    if let Some(result) = unsafe { & *node.array() }.get(index) {
                        return Some(unsafe { result.assume_init_ref() })
                    }
                    debug_assert!(*node != content.last, "Using next from last node despite previous len check.");
                    index -= unsafe { & *node.array() }.len();
                    if let Some(next) = &unsafe { & *node.header() }.next {
                        node = &next;
                    } else {
                        break;
                    }
                }
            }
        }
        None
    }

    /// Get the mutable reference of an element at the specified index.
    #[inline]
    pub fn get_mut(&mut self, mut index: usize) -> Option<&mut T> {
        if let Some(content) = &mut self.content {
            if index < content.len {
                let mut node = &mut content.first;
                loop {
                    debug_assert!(*node != content.last || index < content.last_node_used, "Using an uninitialized node despite previous len check.");
                    if let Some(result) = unsafe { &mut *node.array() }.get_mut(index) {
                        return Some(unsafe { result.assume_init_mut() })
                    }
                    debug_assert!(*node != content.last, "Using next from last node despite previous len check.");
                    index -= unsafe { & *node.array() }.len();
                    if let Some(next) = unsafe { &mut *node.header() }.next.as_mut() {
                        node = next;
                    } else {
                        break;
                    }
                }
            }
        }
        None
    }

    #[cold]
    fn slow_add(arena: &'a A, content: &mut Content<T>) -> Result<(), AllocError> {
        content.last_node_used = 0;
        Ok(if let Some(next) = unsafe { & *content.last.header() }.next {
            content.last = next;
        } else {
            // New node is requested with the same length as the entire linked list.
            // So the nodes sizes tend to be: 0 -> 4 -> 4 -> 8  -> 16 -> 32 -> 64.
            // And the total len of the list: 0 -> 4 -> 8 -> 16 -> 32 -> 64 -> 128.
            let mut next = Self::alloc_node(arena, content.len)?;
            unsafe { next.as_mut() }.header.prev = Some(content.last);
            let next = NodePtr(next.cast(), PhantomData);
            unsafe { &mut *content.last.header() }.next = Some(next);
            content.last = next;
        })
    }

    #[cold]
    fn first_add(&mut self, value: T) -> Result<(), AllocError> {
        // The `usize::max` is to ensure nodes doesn't have a too large memory overhead for their header.
        let mut node = Self::alloc_node(self.arena, usize::max(4, mem::size_of::<usize>() * 5 / mem::size_of::<T>()))?;
        let node_ = unsafe { node.as_mut() };
        node_.header.prev = None;
        node_.header.next = None;
        debug_assert!(node_.array.len() > 0, "Arena returned a zero len array.");
        *unsafe { node_.array.get_unchecked_mut(0) } = MaybeUninit::new(value);
        let node = NodePtr(node.cast(), PhantomData);
        Ok(self.content = Some(Content {
            len: 1,
            last_node_used: 1,
            first: node,
            last: node
        }))
    }

    fn alloc_node(arena: &'a A, len: usize) -> Result<NonNull<Node<T>>, AllocError> {
        let layout = Layout::new::<NodeHeader<T>>()
            .extend(Layout::array::<MaybeUninit<T>>(len).map_err(#[inline(always)] |_| AllocError::InvalidLayout)?)
            .map_err(#[inline(always)] |_| AllocError::InvalidLayout)?;
        let (raw_allocation, len) = if let Some((allocation, len)) = arena.try_alloc_remaining_dst_with_layout(
            Layout::new::<NodeHeader<T>>(),
            Layout::new::<MaybeUninit<T>>(),
            1..len
        ) {
            (Box::into_raw(allocation), len)
        } else {
            let allocation = arena.try_alloc_layout(layout.0)?;
            (Box::into_raw(allocation), len)
        };
        Ok(unsafe { NonNull::new_unchecked(ptr_from_raw_parts_mut!(raw_allocation as *mut (), len)) })
    }
}

/// Represent an iterator over [UnrolledList].
pub struct IteratorRef<'a, T> {
    remaining: usize,
    index: usize,
    chunk: Option<NodePtr<T>>,
    phantom: PhantomData<&'a T>,
}

impl<'a, T: 'a> iter::Iterator for IteratorRef<'a, T> {
    type Item = &'a T;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        return if self.remaining != 0 {
            self.remaining -= 1;
            debug_assert!(self.chunk.is_some(), "Is none despite we have remaining length.");
            let chunk = unsafe { self.chunk.unwrap_unchecked() };
            if let Some(v) = unsafe { & *chunk.array() }.get(self.index) {
                self.index += 1;
                Some(unsafe { v.assume_init_ref() })
            } else {
                slow(self, chunk)
            }
        } else {
            None
        };

        #[cold]
        fn slow<'a, T>(this: &mut IteratorRef<'a, T>, chunk: NodePtr<T>) -> Option<&'a T> {
            this.index = 1;
            this.chunk = unsafe { & *chunk.header() }.next;
            debug_assert!(this.chunk.is_some(), "Is none despite we have remaining length.");
            let array = unsafe { & *this.chunk.unwrap_unchecked().array() };
            debug_assert!(array.len() > 0, "No array can be empty.");
            Some(unsafe { array.get_unchecked(0).assume_init_ref() })
        }
    }
}
/// Represent an iterator over [UnrolledList].
pub struct IteratorMut<'a, T> {
    remaining: usize,
    index: usize,
    chunk: Option<NodePtr<T>>,
    phantom: PhantomData<&'a mut T>,
}

impl<'a, T: 'a> iter::Iterator for IteratorMut<'a, T> {
    type Item = &'a mut T;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        return if self.remaining != 0 {
            self.remaining -= 1;
            debug_assert!(self.chunk.is_some(), "Is none despite we have remaining length.");
            let chunk = unsafe { self.chunk.unwrap_unchecked() };
            if let Some(v) = unsafe { &mut *chunk.array() }.get_mut(self.index) {
                self.index += 1;
                Some(unsafe { v.assume_init_mut() })
            } else {
                slow(self, chunk)
            }
        } else {
            None
        };

        #[cold]
        fn slow<'a, T>(this: &mut IteratorMut<'a, T>, chunk: NodePtr<T>) -> Option<&'a mut T> {
            this.index = 1;
            this.chunk = unsafe { & *chunk.header() }.next;
            debug_assert!(this.chunk.is_some(), "Is none despite we have remaining length.");
            let array = unsafe { &mut *this.chunk.unwrap_unchecked().array() };
            debug_assert!(array.len() > 0, "No array can be empty.");
            Some(unsafe { array.get_unchecked_mut(0).assume_init_mut() })
        }
    }
}

/// Represent an iterator over [UnrolledList].
pub struct Iterator<'a, T> {
    remaining: usize,
    index: usize,
    chunk: Option<NodePtr<T>>,
    phantom: PhantomData<&'a ()>,
}

impl<'a, T> iter::Iterator for Iterator<'a, T> {
    type Item = T;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        return if self.remaining != 0 {
            self.remaining -= 1;
            debug_assert!(self.chunk.is_some(), "Is none despite we have remaining length.");
            let chunk = unsafe { self.chunk.unwrap_unchecked() };
            if let Some(v) = unsafe { & *chunk.array() }.get(self.index) {
                self.index += 1;
                Some(unsafe { (v.assume_init_ref() as *const T).read() })
            } else {
                slow(self, chunk)
            }
        } else {
            None
        };

        #[cold]
        fn slow<'a, T>(this: &mut Iterator<'a, T>, chunk: NodePtr<T>) -> Option<T> {
            this.index = 1;
            this.chunk = unsafe { & *chunk.header() }.next;
            debug_assert!(this.chunk.is_some(), "Is none despite we have remaining length.");
            let array = unsafe { & *this.chunk.unwrap_unchecked().array() };
            debug_assert!(array.len() > 0, "No array can be empty.");
            Some(unsafe { (array.get_unchecked(0).assume_init_ref() as *const T).read() })
        }
    }
}

impl<'a, T> Drop for Iterator<'a, T> {
    fn drop(&mut self) {
        // Drop elements.
        while let Some(e) = self.next() {
            drop(e)
        }
    }
}

impl<'a, T, A: Arena> IntoIterator for UnrolledList<'a, T, A> {
    type Item = T;

    type IntoIter = Iterator<'a, T>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        let this = ManuallyDrop::new(self);
        if let Some(content) = &this.content {
            Iterator {
                remaining: content.len,
                index: 0,
                chunk: Some(content.first),
                phantom: PhantomData
            }
        } else {
            Iterator { remaining: 0, index: 0, chunk: None, phantom: PhantomData }
        }
    }
}