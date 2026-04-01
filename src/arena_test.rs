use std::{fmt, mem, alloc::Layout};

use crate::{AllocError, Box, BuilderDST, InsertingOrder};

pub(crate) fn assert_value<'a, T: PartialEq + fmt::Debug + ?Sized + 'a>(f: impl FnOnce(&T) -> Result<Box<'a, T>, AllocError>, value: &T) {
    let slot = f(value);
    assert_eq!(slot.as_deref(), Ok(value), "Invalid value.");
    let ptr = Box::into_raw(slot.unwrap());
    assert_eq!(ptr.addr() % unsafe { mem::align_of_val_raw(ptr) }, 0, "Invalid alignment.");
}

pub(crate) fn assert_length<'a, T: 'a>(f: impl FnOnce(usize) -> Box<'a, [T]>, len: usize) {
    let slot = f(len);
    let ptr = Box::as_ptr(&slot);
    assert_eq!(slot.len(), len, "Invalid length.");
    assert_eq!(ptr.addr() % unsafe { mem::align_of_val_raw(ptr) }, 0, "Invalid alignment.");
}

pub(crate) fn assert_length2<'a, T: 'a>(f: impl FnOnce(usize) -> (Box<'a, [T]>, usize), len1: usize, len2: usize) {
    let slot = f(len2);
    let ptr = Box::as_ptr(&slot.0);
    assert_eq!(slot.0.len(), len1, "Invalid length.");
    assert_eq!(slot.1, len2, "Invalid length.");
    assert_eq!(ptr.addr() % unsafe { mem::align_of_val_raw(ptr) }, 0, "Invalid alignment.");
}

pub(crate) fn assert_align<'a, T: ?Sized + 'a>(f: impl FnOnce() -> Result<Box<'a, T>, AllocError>) {
    let slot = f();
    let value = slot.unwrap();
    let ptr = Box::as_ptr(&value);
    assert_eq!(ptr.addr() % unsafe { mem::align_of_val_raw(ptr) }, 0, "Invalid alignment.");
}

pub(crate) struct Builder {
    pub(crate) header: Layout,
    pub(crate) element: Layout,
    pub(crate) count: usize,
    pub(crate) len: usize,
    pub(crate) inserting_order: InsertingOrder,
    pub(crate) elements_hint: (usize, Option<usize>),
    pub(crate) header_w: bool,
    pub(crate) finalize: bool,
}

impl BuilderDST for Builder {
    fn header_layout(&self) -> Layout {
        self.header
    }

    fn element_layout(&self) -> Layout {
        self.element
    }

    fn inserting_order(&self) -> InsertingOrder {
        self.inserting_order
    }

    fn elements_hint(&self) -> (usize, Option<usize>) {
        self.elements_hint
    }

    fn write_header(&mut self, memory: &mut [u8]) -> bool {
        if memory.len() > 0 {
            memory[0] = 128;
        }
        self.header_w
    }

    fn write_element(&mut self, memory: &mut [u8]) -> bool {
        if self.count == self.len {
            self.count = 0;
            false
        } else {
            if memory.len() > 0 {
                memory[0] = self.count as u8;
            }
            self.count += 1;
            true
        }
    }

    fn finalizer(&mut self, _: &mut [u8]) -> bool {
        self.finalize
    }

    fn drop_header(&mut self, memory: &mut [u8]) {
        if memory.len() > 0 {
            assert_eq!(memory[0], 128);
        }
    }

    fn drop_element(&mut self, memory: &mut [u8]) {
        if memory.len() > 0 {
            assert_eq!(memory[0], self.count as u8);
            self.count += 1;
        }
    }
}

macro_rules! arena_test_ {
    ($a:ident) => {
        use std::{alloc::Layout, mem};

        use crate::{Arena, CancellationError, InsertingOrder, WriteElementState, arena_test::*};

        #[test]
        fn try_alloc() {
            let shared = SharedArena::default();
            let arena = $a(shared.make_local());

            assert_value(|e| arena.try_alloc(*e), &151u8);
            assert_value(|e| arena.try_alloc(*e), &314u16);
            assert_value(|e| arena.try_alloc(*e), &1541564u32);
            assert_value(|e| arena.try_alloc(*e), &71154u64);
            assert_value(|e| arena.try_alloc(*e), &15641.0215f32);
            assert_value(|e| arena.try_alloc(*e), &97984.6789851f64);
            assert_value(|e| arena.try_alloc(*e), &[7, 9, 1, 3, 4, 7, 8, 6]);
        }

        #[test]
        #[cfg(feature = "clone_to_uninit")]
        fn try_alloc_from_clone() {
            let shared = SharedArena::default();
            let arena = $a(shared.make_local());

            assert_value(|e| arena.try_alloc_from_clone(e), &151u8);
            assert_value(|e| arena.try_alloc_from_clone(e), &314u16);
            assert_value(|e| arena.try_alloc_from_clone(e), &1541564u32);
            assert_value(|e| arena.try_alloc_from_clone(e), &71154u64);
            assert_value(|e| arena.try_alloc_from_clone(e), &15641.0215f32);
            assert_value(|e| arena.try_alloc_from_clone(e), &97984.6789851f64);
            assert_value(|e| arena.try_alloc_from_clone(e), &[7, 9, 1, 3, 4, 7, 8, 6]);
        }

        #[test]
        fn try_alloc_slice_copy_clone_str() {
            let shared = SharedArena::default();
            let arena = $a(shared.make_local());

            assert_value::<[i32]>(|e| arena.try_alloc_slice_copy(&e), &[7, 8, 3, 4, 6]);
            assert_value::<[i32]>(|e| arena.try_alloc_slice_clone(&e), &[3, 9, 7, 1, 6, 4, 515]);
            assert_value(|e| arena.try_alloc_str(&e), "safsdf");
        }

        #[test]
        fn try_alloc_slice_fill_copy_clone_default_iter() {
            let shared = SharedArena::default();
            let arena = $a(shared.make_local());

            assert_value::<[i32]>(|_| arena.try_alloc_slice_fill_copy(6, &5), &[5, 5, 5, 5, 5, 5]);
            assert_value::<[i32]>(|_| arena.try_alloc_slice_fill_clone(6, &5), &[5, 5, 5, 5, 5, 5]);
            assert_value::<[i32]>(|_| arena.try_alloc_slice_fill_default(6), &[0, 0, 0, 0, 0, 0]);
            assert_value::<[i32]>(|_| arena.try_alloc_slice_fill_iter(5..8), &(5..8).into_iter().collect::<Vec<_>>());
        }

        #[test]
        fn try_alloc_uninit() {
            let shared = SharedArena::default();
            let arena = $a(shared.make_local());

            assert_align(|| arena.try_alloc_slice::<i32>(5));
            assert_align(|| arena.try_alloc_uninit::<i32>());
            assert_align(|| arena.try_alloc_layout(Layout::new::<i32>()));
        }

        #[test]
        fn try_alloc_remaining_dst_with_builder() {
            let shared = SharedArena::default();
            let mut arena = $a(shared.make_local());

            arena.reset();
            assert_length(|_| {
                let result = arena.try_alloc_remaining_dst_with_builder(Builder {
                    header: Layout::new::<()>(),
                    element: Layout::new::<()>(),
                    len: 50,
                    inserting_order: InsertingOrder::Unspecified,
                    elements_hint: (0, None),
                    header_w: true,
                    finalize: true,
                    count: 0,
                });
                let (allocation, write_element) = result.unwrap();
                assert_eq!(write_element, WriteElementState::Started { count: 50, completed: true });
                allocation
            }, 0);
            assert_length(|_| {
                let result = arena.try_alloc_remaining_dst_with_builder(Builder {
                    header: Layout::new::<()>(),
                    element: Layout::new::<u16>(),
                    len: 50,
                    inserting_order: InsertingOrder::Unspecified,
                    elements_hint: (0, None),
                    header_w: true,
                    finalize: true,
                    count: 0,
                });
                let (allocation, write_element) = result.unwrap();
                assert_eq!(write_element, WriteElementState::NeverStarted);
                allocation
            }, 0);
            {
                let result = arena.try_alloc_remaining_dst_with_builder(Builder {
                    header: Layout::new::<u16>(),
                    element: Layout::new::<u16>(),
                    len: 50,
                    inserting_order: InsertingOrder::Unspecified,
                    elements_hint: (0, None),
                    header_w: true,
                    finalize: true,
                    count: 0,
                });
                assert_eq!(result.unwrap_err(), CancellationError::CancelledBeforeWrite);
            }
            {
                let result = arena.try_alloc_remaining_dst_with_builder(Builder {
                    header: Layout::new::<()>(),
                    element: Layout::new::<u16>(),
                    len: 50,
                    inserting_order: InsertingOrder::Unspecified,
                    elements_hint: (usize::MAX, None),
                    header_w: true,
                    finalize: true,
                    count: 0,
                });
                assert_eq!(result.unwrap_err(), CancellationError::CancelledBeforeWrite);
            }

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert_length(|_| {
                let result = arena.try_alloc_remaining_dst_with_builder(Builder {
                    header: Layout::new::<()>(),
                    element: Layout::new::<()>(),
                    len: 50,
                    inserting_order: InsertingOrder::Unspecified,
                    elements_hint: (0, None),
                    header_w: true,
                    finalize: true,
                    count: 0,
                });
                let (allocation, write_element) = result.unwrap();
                assert_eq!(write_element, WriteElementState::Started { count: 50, completed: true });
                allocation
            }, 0);

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            {
                let result = arena.try_alloc_remaining_dst_with_builder(Builder {
                    header: Layout::new::<u32>(),
                    element: Layout::new::<u16>(),
                    len: 50,
                    inserting_order: InsertingOrder::Unspecified,
                    elements_hint: (usize::MAX, None),
                    header_w: true,
                    finalize: true,
                    count: 0,
                });
                assert_eq!(result.unwrap_err(), CancellationError::CancelledBeforeWrite);
            }

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            {
                let result = arena.try_alloc_remaining_dst_with_builder(Builder {
                    header: Layout::new::<u32>(),
                    element: Layout::new::<u16>(),
                    len: 50,
                    inserting_order: InsertingOrder::Unspecified,
                    elements_hint: (0, None),
                    header_w: false,
                    finalize: true,
                    count: 0,
                });
                assert_eq!(result.unwrap_err(), CancellationError::CancelledByHeader(WriteElementState::Started { count: 50, completed: true }));
            }

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            {
                let result = arena.try_alloc_remaining_dst_with_builder(Builder {
                    header: Layout::new::<u32>(),
                    element: Layout::new::<u16>(),
                    len: 50,
                    inserting_order: InsertingOrder::Unspecified,
                    elements_hint: (0, None),
                    header_w: true,
                    finalize: false,
                    count: 0,
                });
                assert_eq!(result.unwrap_err(), CancellationError::CancelledByFinalizer(WriteElementState::Started { count: 50, completed: true }));
            }

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            {
                let result = arena.try_alloc_remaining_dst_with_builder(Builder {
                    header: Layout::new::<u32>(),
                    element: Layout::new::<u16>(),
                    len: 50,
                    inserting_order: InsertingOrder::Original,
                    elements_hint: (0, None),
                    header_w: true,
                    finalize: false,
                    count: 0,
                });
                assert_eq!(result.unwrap_err(), CancellationError::CancelledByFinalizer(WriteElementState::Started { count: 50, completed: true }));
            }

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            {
                let result = arena.try_alloc_remaining_dst_with_builder(Builder {
                    header: Layout::new::<u32>(),
                    element: Layout::new::<u16>(),
                    len: 50,
                    inserting_order: InsertingOrder::Reverse,
                    elements_hint: (0, None),
                    header_w: true,
                    finalize: false,
                    count: 0,
                });
                assert_eq!(result.unwrap_err(), CancellationError::CancelledByFinalizer(WriteElementState::Started { count: 50, completed: true }));
            }

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert_length(|_| {
                let result = arena.try_alloc_remaining_dst_with_builder(Builder {
                    header: Layout::new::<()>(),
                    element: Layout::new::<u16>(),
                    len: 50,
                    inserting_order: InsertingOrder::Unspecified,
                    elements_hint: (0, None),
                    header_w: true,
                    finalize: true,
                    count: 0,
                });
                let (allocation, write_element) = result.unwrap();
                assert_eq!(write_element, WriteElementState::Started { count: 50, completed: true });
                allocation
            }, Layout::new::<()>().extend(Layout::array::<u16>(50).unwrap()).unwrap().0.pad_to_align().size());

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert_length(|_| {
                let result = arena.try_alloc_remaining_dst_with_builder(Builder {
                    header: Layout::new::<u32>(),
                    element: Layout::new::<u16>(),
                    len: 50,
                    inserting_order: InsertingOrder::Unspecified,
                    elements_hint: (0, None),
                    header_w: true,
                    finalize: true,
                    count: 0,
                });
                let (allocation, write_element) = result.unwrap();
                assert_eq!(write_element, WriteElementState::Started { count: 50, completed: true });
                allocation
            }, Layout::new::<u32>().extend(Layout::array::<u16>(50).unwrap()).unwrap().0.pad_to_align().size());

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert_length(|_| {
                let result = arena.try_alloc_remaining_dst_with_builder(Builder {
                    header: Layout::new::<u32>(),
                    element: Layout::new::<u16>(),
                    len: 50,
                    inserting_order: InsertingOrder::Original,
                    elements_hint: (0, None),
                    header_w: true,
                    finalize: true,
                    count: 0,
                });
                let (allocation, write_element) = result.unwrap();
                assert_eq!(write_element, WriteElementState::Started { count: 50, completed: true });
                allocation
            }, Layout::new::<u32>().extend(Layout::array::<u16>(50).unwrap()).unwrap().0.pad_to_align().size());

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert_length(|_| {
                let result = arena.try_alloc_remaining_dst_with_builder(Builder {
                    header: Layout::new::<u32>(),
                    element: Layout::new::<u16>(),
                    len: 50,
                    inserting_order: InsertingOrder::Reverse,
                    elements_hint: (0, None),
                    header_w: true,
                    finalize: true,
                    count: 0,
                });
                let (allocation, write_element) = result.unwrap();
                assert_eq!(write_element, WriteElementState::Started { count: 50, completed: true });
                allocation
            }, Layout::new::<u32>().extend(Layout::array::<u16>(50).unwrap()).unwrap().0.pad_to_align().size());

            arena.reset();
            {
                let result = arena.try_alloc_remaining_dst_with_builder(Builder {
                    header: Layout::new::<u32>(),
                    element: Layout::new::<u16>(),
                    len: 50,
                    inserting_order: InsertingOrder::Unspecified,
                    elements_hint: (0, None),
                    header_w: true,
                    finalize: false,
                    count: 0,
                });
                assert_eq!(result.unwrap_err(), CancellationError::CancelledByFinalizer(WriteElementState::Started { count: 50, completed: true }));
            }
            assert_align(|| {
                let result = arena.try_alloc_remaining_dst_with_builder(Builder {
                    header: Layout::new::<u32>(),
                    element: Layout::new::<u16>(),
                    len: 50000,
                    inserting_order: InsertingOrder::Original,
                    elements_hint: (0, None),
                    header_w: true,
                    finalize: true,
                    count: 0,
                });
                let (allocation, write_element) = result.unwrap();
                assert!(matches!(write_element, WriteElementState::Started { count: _, completed: false }));
                Ok(allocation)
            });
            assert_value(|e| arena.try_alloc(*e), &1u8);

            arena.reset();
            assert_align(|| {
                let result = arena.try_alloc_remaining_dst_with_builder(Builder {
                    header: Layout::new::<u32>(),
                    element: Layout::new::<u16>(),
                    len: 50000,
                    inserting_order: InsertingOrder::Original,
                    elements_hint: (0, None),
                    header_w: true,
                    finalize: true,
                    count: 0,
                });
                let (allocation, write_element) = result.unwrap();
                assert!(matches!(write_element, WriteElementState::Started { count: _, completed: false }));
                Ok(allocation)
            });
            assert_align(|| {
                let result = arena.try_alloc_remaining_dst_with_builder(Builder {
                    header: Layout::new::<u32>(),
                    element: Layout::new::<u16>(),
                    len: 50000,
                    inserting_order: InsertingOrder::Original,
                    elements_hint: (0, None),
                    header_w: true,
                    finalize: true,
                    count: 0,
                });
                let (allocation, write_element) = result.unwrap();
                assert!(matches!(write_element, WriteElementState::Started { count: _, completed: false }));
                Ok(allocation)
            });
        }

        #[test]
        fn try_alloc_remaining_dst_with_layout() {
            let shared = SharedArena::default();
            let mut arena = $a(shared.make_local());

            arena.reset();

            assert_length2(|e| arena.try_alloc_remaining_dst_with_layout(Layout::new::<()>(), Layout::new::<()>(), 50..e).unwrap(), 0, 50);
            assert_length2(|e| arena.try_alloc_remaining_dst_with_layout(Layout::new::<()>(), Layout::new::<()>(), 0..e).unwrap(), 0, 50);
            assert_length2(|_| arena.try_alloc_remaining_dst_with_layout(Layout::new::<()>(), Layout::new::<u16>(), 0..50).unwrap(), 0, 0);
            assert_length2(|_| arena.try_alloc_remaining_dst_with_layout(Layout::new::<()>(), Layout::new::<u16>(), 0..50).unwrap(), 0, 0);
            assert_length2(|_| arena.try_alloc_remaining_dst_with_layout(Layout::new::<()>(), Layout::new::<u16>(), 0..0).unwrap(), 0, 0);

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert_length2(|e| arena.try_alloc_remaining_dst_with_layout(Layout::new::<()>(), Layout::new::<()>(), 50..e).unwrap(), 0, 50);
            assert_length2(|e| arena.try_alloc_remaining_dst_with_layout(Layout::new::<()>(), Layout::new::<()>(), 0..e).unwrap(), 0, 50);
            assert_length2(|_| arena.try_alloc_remaining_dst_with_layout(Layout::new::<()>(), Layout::new::<u16>(), 0..0).unwrap(), 0, 0);

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert_length2(|e| arena.try_alloc_remaining_dst_with_layout(Layout::new::<()>(), Layout::new::<u16>(), 0..e).unwrap(), 50, 50 / mem::size_of::<u16>());

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert_length2(|e| arena.try_alloc_remaining_dst_with_layout(Layout::new::<u16>(), Layout::new::<()>(), e..e).unwrap(), 2, 50);

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert_length2(|_| arena.try_alloc_remaining_dst_with_layout(Layout::new::<()>(), Layout::new::<u16>(), 0..0).unwrap(), 0, 0);

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert_length2(|e| arena.try_alloc_remaining_dst_with_layout(Layout::new::<u16>(), Layout::new::<u8>(), e..e).unwrap(), Layout::new::<u16>().extend(Layout::array::<u8>(50).unwrap()).unwrap().0.size(), 50);

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert!(arena.try_alloc_remaining_dst_with_layout(Layout::new::<u16>(), Layout::new::<u8>(), 50000..50000).is_none());
            assert_align(|| Ok(arena.try_alloc_remaining_dst_with_layout(Layout::new::<u16>(), Layout::new::<u8>(), 0..50000).unwrap().0));
            assert_length2(|_| arena.try_alloc_remaining_dst_with_layout(Layout::new::<()>(), Layout::new::<u8>(), 0..50000).unwrap(), 0, 0);
            assert!(arena.try_alloc_remaining_dst_with_layout(Layout::new::<()>(), Layout::new::<u8>(), 1..50000).is_none());

            arena.reset();
            assert!(arena.try_alloc_remaining_dst_with_layout(Layout::new::<u16>(), Layout::new::<u8>(), 50000..50000).is_none());
            assert_length(|_| arena.try_alloc_remaining_dst_with_layout(Layout::new::<()>(), Layout::new::<u8>(), 0..50000).unwrap().0, 0);
            assert_align(|| Ok(arena.try_alloc_remaining_dst_with_layout(Layout::new::<()>(), Layout::new::<u8>(), 1..50000).unwrap().0));

            assert_value(|e| arena.try_alloc(*e), &1u8);
            arena.reset();
            assert_align(|| Ok(arena.try_alloc_remaining_dst_with_layout(Layout::new::<()>(), Layout::new::<u8>(), 1..50000).unwrap().0));
            assert_align(|| Ok(arena.try_alloc_remaining_dst_with_layout(Layout::new::<()>(), Layout::new::<u8>(), 1..50000).unwrap().0));
        }

        #[test]
        fn try_alloc_remaining_slice_with_layout() {
            let shared = SharedArena::default();
            let mut arena = $a(shared.make_local());

            arena.reset();
            assert_length2(|e| arena.try_alloc_remaining_slice_with_layout(Layout::new::<()>(), 50..e).unwrap(), 0, 50);
            assert_length2(|e| arena.try_alloc_remaining_slice_with_layout(Layout::new::<()>(), 0..e).unwrap(), 0, 50);
            assert_length2(|_| arena.try_alloc_remaining_slice_with_layout(Layout::new::<u16>(), 0..0).unwrap(), 0, 0);

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert_length2(|e| arena.try_alloc_remaining_slice_with_layout(Layout::new::<()>(), 50..e).unwrap(), 0, 50);
            assert_length2(|e| arena.try_alloc_remaining_slice_with_layout(Layout::new::<()>(), 0..e).unwrap(), 0, 50);
            assert_length2(|_| arena.try_alloc_remaining_slice_with_layout(Layout::new::<u16>(), 0..0).unwrap(), 0, 0);

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert_length2(|e| arena.try_alloc_remaining_slice_with_layout(Layout::new::<u16>(), 0..e).unwrap(), 50 * mem::size_of::<u16>(), 50);

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert!(arena.try_alloc_remaining_slice_with_layout(Layout::new::<u16>(), 50000..50000).is_none());
            assert_align(|| Ok(arena.try_alloc_remaining_slice_with_layout(Layout::new::<u16>(), 0..50000).unwrap().0));
            assert_length2(|_| arena.try_alloc_remaining_slice_with_layout(Layout::new::<u16>(), 0..50000).unwrap(), 0, 0);
            assert!(arena.try_alloc_remaining_slice_with_layout(Layout::new::<u16>(), 1..50000).is_none());

            arena.reset();
            assert!(arena.try_alloc_remaining_slice_with_layout(Layout::new::<u16>(), 50000..50000).is_none());
            assert_length2(|_| arena.try_alloc_remaining_slice_with_layout(Layout::new::<u16>(), 0..50000).unwrap(), 0, 0);
            assert_align(|| Ok(arena.try_alloc_remaining_slice_with_layout(Layout::new::<u16>(), 1..50000).unwrap().0));

            assert_value(|e| arena.try_alloc(*e), &1u8);
            arena.reset();
            assert_align(|| Ok(arena.try_alloc_remaining_slice_with_layout(Layout::new::<u16>(), 1..50000).unwrap().0));
            assert_align(|| Ok(arena.try_alloc_remaining_slice_with_layout(Layout::new::<u16>(), 1..50000).unwrap().0));
        }

        #[test]
        fn alloc_remaining_slice_with_layout() {
            let shared = SharedArena::default();
            let mut arena = $a(shared.make_local());

            arena.reset();
            assert_length2(|e| arena.alloc_remaining_slice_with_layout(Layout::new::<()>(), e), 0, 50);
            assert_length2(|_| arena.alloc_remaining_slice_with_layout(Layout::new::<u16>(), 0), 0, 0);

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert_length2(|e| arena.alloc_remaining_slice_with_layout(Layout::new::<()>(), e), 0, 50);
            assert_length2(|_| arena.alloc_remaining_slice_with_layout(Layout::new::<u16>(), 0), 0, 0);

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert_length2(|e| arena.alloc_remaining_slice_with_layout(Layout::new::<u16>(), e), 50 * mem::size_of::<u16>(), 50);

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert_align(|| Ok(arena.alloc_remaining_slice_with_layout(Layout::new::<u16>(), 50000).0));
            assert_length2(|_| arena.alloc_remaining_slice_with_layout(Layout::new::<u16>(), 50000), 0, 0);

            assert_value(|e| arena.try_alloc(*e), &1u8);
            arena.reset();
            assert_align(|| Ok(arena.alloc_remaining_slice_with_layout(Layout::new::<u16>(), 50000).0));
            assert_align(|| Ok(arena.alloc_remaining_slice_with_layout(Layout::new::<u16>(), 50000).0));
        }

        #[test]
        fn alloc_remaining_slice_from_iter_with_order() {
            let shared = SharedArena::default();
            let mut arena = $a(shared.make_local());

            arena.reset();
            assert_align(|| {
                let (slice, remaining) = arena.alloc_remaining_slice_from_iter_with_order((5..50u16).into_iter(), InsertingOrder::Original);
                assert_ne!(remaining, None);
                Ok(slice)
            });
            assert_length(|e| {
                let (slice, remaining) = arena.alloc_remaining_slice_from_iter_with_order((0..e).into_iter().map(|_| ()), InsertingOrder::Unspecified);
                assert!(remaining.is_none());
                slice
            }, 50);

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert_value::<[u16]>(|e| {
                let (slice, remaining) = arena.alloc_remaining_slice_from_iter_with_order(e.iter().copied(), InsertingOrder::Original);
                assert!(remaining.is_none());
                Ok(slice)
            }, &(5..50).into_iter().collect::<Vec<_>>());
            assert_align(|| {
                let (slice, remaining) = arena.alloc_remaining_slice_from_iter_with_order((0..100000000).into_iter(), InsertingOrder::Original);
                assert_ne!(remaining, None);
                Ok(slice)
            });
            assert_length(|_| {
                let (slice, remaining) = arena.alloc_remaining_slice_from_iter_with_order((0..100000000).into_iter(), InsertingOrder::Original);
                assert_ne!(remaining, None);
                slice
            }, 0);
            assert_length(|e| {
                let (slice, remaining) = arena.alloc_remaining_slice_from_iter_with_order((0..e).into_iter().map(|_| ()), InsertingOrder::Unspecified);
                assert!(remaining.is_none());
                slice
            }, 50);

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert_value::<[u16]>(|_| {
                let (slice, remaining) = arena.alloc_remaining_slice_from_iter_with_order((5..50).into_iter().rev(), InsertingOrder::Reverse);
                assert!(remaining.is_none());
                Ok(slice)
            }, &(5..50).into_iter().collect::<Vec<_>>());
        }

        #[test]
        fn alloc_remaining_slice_from_iter() {
            let shared = SharedArena::default();
            let mut arena = $a(shared.make_local());

            arena.reset();
            assert_align(|| {
                let (slice, remaining) = arena.alloc_remaining_slice_from_iter((5..50u16).into_iter());
                assert_ne!(remaining, None);
                Ok(slice)
            });
            assert_length(|e| {
                let (slice, remaining) = arena.alloc_remaining_slice_from_iter((0..e).into_iter().map(|_| ()));
                assert!(remaining.is_none());
                slice
            }, 50);

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert_value::<[u16]>(|e| {
                let (slice, remaining) = arena.alloc_remaining_slice_from_iter(e.iter().copied());
                assert!(remaining.is_none());
                Ok(slice)
            }, &(5..50).into_iter().collect::<Vec<_>>());
            assert_align(|| {
                let (slice, remaining) = arena.alloc_remaining_slice_from_iter((0..100000000).into_iter());
                assert_ne!(remaining, None);
                Ok(slice)
            });
            assert_length(|_| {
                let (slice, remaining) = arena.alloc_remaining_slice_from_iter((0..100000000).into_iter());
                assert_ne!(remaining, None);
                slice
            }, 0);
            assert_length(|e| {
                let (slice, remaining) = arena.alloc_remaining_slice_from_iter((0..e).into_iter().map(|_| ()));
                assert!(remaining.is_none());
                slice
            }, 50);

            assert_value(|e| arena.try_alloc(*e), &1u8);
            arena.reset();
            assert_align(|| {
                let (slice, remaining) = arena.alloc_remaining_slice_from_iter((0..100000000).into_iter());
                assert_ne!(remaining, None);
                Ok(slice)
            });
            assert_align(|| {
                let (slice, remaining) = arena.alloc_remaining_slice_from_iter((0..100000000).into_iter());
                assert_ne!(remaining, None);
                Ok(slice)
            });
        }

        #[test]
        fn try_alloc_remaining_slice() {
            let shared = SharedArena::default();
            let mut arena = $a(shared.make_local());

            arena.reset();
            assert_length(|e| arena.try_alloc_remaining_slice::<()>(50..e).unwrap(), 50);
            assert_length(|e| arena.try_alloc_remaining_slice::<()>(0..e).unwrap(), 50);
            assert_length(|_| arena.try_alloc_remaining_slice::<u16>(0..0).unwrap(), 0);
            assert_length(|_| arena.try_alloc_remaining_slice::<u16>(0..50).unwrap(), 0);

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert_length(|e| arena.try_alloc_remaining_slice::<()>(50..e).unwrap(), 50);
            assert_length(|e| arena.try_alloc_remaining_slice::<()>(0..e).unwrap(), 50);
            assert_length(|_| arena.try_alloc_remaining_slice::<u16>(0..0).unwrap(), 0);

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert_length(|e| arena.try_alloc_remaining_slice::<u16>(0..e).unwrap(), 50);

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert!(arena.try_alloc_remaining_slice::<u16>(50000..50000).is_none());
            assert_align(|| Ok(arena.try_alloc_remaining_slice::<u16>(0..50000).unwrap()));
            assert_length(|_| arena.try_alloc_remaining_slice::<u16>(0..50000).unwrap(), 0);
            assert!(arena.try_alloc_remaining_slice::<u16>(1..50000).is_none());

            arena.reset();
            assert!(arena.try_alloc_remaining_slice::<u16>(50000..50000).is_none());
            assert_length(|_| arena.try_alloc_remaining_slice::<u16>(0..50000).unwrap(), 0);
            assert_align(|| Ok(arena.try_alloc_remaining_slice::<u16>(1..50000).unwrap()));
        }

        #[test]
        fn alloc_slice_from_remaining() {
            let shared = SharedArena::default();
            let mut arena = $a(shared.make_local());

            arena.reset();
            assert_length(|e| arena.alloc_slice_from_remaining::<()>(e), 50);
            assert_length(|_| arena.alloc_slice_from_remaining::<u16>(0), 0);
            assert_length(|_| arena.alloc_slice_from_remaining::<u16>(50), 0);

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert_length(|e| arena.alloc_slice_from_remaining::<()>(e), 50);
            assert_length(|_| arena.alloc_slice_from_remaining::<u16>(0), 0);

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert_length(|e| arena.alloc_slice_from_remaining::<u16>(e), 100);

            arena.reset();
            assert_value(|e| arena.try_alloc(*e), &1u8);
            assert_align(|| Ok(arena.alloc_slice_from_remaining::<u16>(50000)));
            assert_length(|_| arena.alloc_slice_from_remaining::<u16>(50000), 0);

            arena.reset();
            assert_length(|_| arena.alloc_slice_from_remaining::<u16>(50000), 0);
        }

        #[test]
        fn zst() {
            let shared = SharedArena::default();
            let arena = $a(shared.make_local());

            assert_value(|e| arena.try_alloc(*e), &());
            assert_value::<[()]>(|e| arena.try_alloc_slice_clone(e), &[(), (), (), (), ()]);
            assert_value::<[()]>(|e| arena.try_alloc_slice_copy(e), &[(), (), (), (), ()]);
            assert_value::<[()]>(|_| arena.try_alloc_slice_fill_clone(5, &()), &[(), (), (), (), ()]);
            assert_value::<[()]>(|_| arena.try_alloc_slice_fill_copy(5, &()), &[(), (), (), (), ()]);
            assert_value::<[()]>(|_| arena.try_alloc_slice_fill_default(5), &[(), (), (), (), ()]);
        }

        #[test]
        fn large_alloc_reuse_and_info() {
            let shared = SharedArena::default();
            let mut arena = $a(shared.make_local());

            arena.reset();

            arena.allocation_info();
            arena.remaining_chunk_capacity();

            assert_value::<[i32]>(|_| arena.try_alloc_slice_fill_iter(5..880), &(5..880).into_iter().collect::<Vec<_>>());
            arena.allocation_info();
            arena.remaining_chunk_capacity();

            assert_value::<[i16]>(|_| arena.try_alloc_slice_fill_iter(5..880), &(5..880).into_iter().collect::<Vec<_>>());
            arena.allocation_info();
            arena.remaining_chunk_capacity();

            assert_value::<[i32]>(|_| arena.try_alloc_slice_fill_iter(5..880), &(5..880).into_iter().collect::<Vec<_>>());
            arena.allocation_info();
            arena.remaining_chunk_capacity();

            assert_value::<[i32]>(|_| arena.try_alloc_slice_fill_iter(5..880), &(5..880).into_iter().collect::<Vec<_>>());
            arena.allocation_info();
            arena.remaining_chunk_capacity();

            arena.reset();

            assert_value::<[i32]>(|_| arena.try_alloc_slice_fill_iter(5..880), &(5..880).into_iter().collect::<Vec<_>>());
            assert_value::<[i16]>(|_| arena.try_alloc_slice_fill_iter(5..880), &(5..880).into_iter().collect::<Vec<_>>());
            assert_value::<[i32]>(|_| arena.try_alloc_slice_fill_iter(5..880), &(5..880).into_iter().collect::<Vec<_>>());
            assert_value::<[i32]>(|_| arena.try_alloc_slice_fill_iter(5..880), &(5..880).into_iter().collect::<Vec<_>>());

            arena.reset();
            assert_value::<[i32]>(|_| arena.try_alloc_slice_fill_iter(5..50000), &(5..50000).into_iter().collect::<Vec<_>>());
        }
    };
}

pub(crate) use arena_test_;
