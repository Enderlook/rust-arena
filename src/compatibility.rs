use std::{mem::MaybeUninit, ptr::{self, NonNull}};

macro_rules! ptr_from_raw_parts_mut {
    ($p:expr, $l:expr) => { ptr_from_raw_parts_mut!($p, $l, _) };
    ($p:expr, $l:expr, $t:ty) => {
        {
            #[cfg(feature = "ptr_metadata")]
            { ptr::from_raw_parts_mut::<$t>($p, $l) }
            #[cfg(not(feature = "ptr_metadata"))]
            { $p.cast::<()>().cast_slice_($l) as *mut $t }
        }
    };
}

pub(crate) use ptr_from_raw_parts_mut;

macro_rules! nonnull_from_raw_parts {
    ($p:expr, $l:expr) => { nonnull_from_raw_parts!($p, $l, _) };
    ($p:expr, $l:expr, $t:ty) => {
        {
            #[cfg(feature = "ptr_metadata")]
            { NonNull::<$t>::from_raw_parts($p, $l) }
            #[cfg(not(feature = "ptr_metadata"))]
            {
                let v = $p.as_ptr().cast::<()>().cast_slice_($l) as *mut $t;
                #[allow(unused_unsafe)]
                unsafe { NonNull::new_unchecked(v) }
            }
        }
    };
}

pub(crate) use nonnull_from_raw_parts;

macro_rules! ptr_metadata {
    ($p:expr) => {
        {
            #[cfg(feature = "ptr_metadata")]
            { ptr::metadata($p) }
            #[cfg(not(feature = "ptr_metadata"))]
            { ($p as *mut [()]).len() }
        }
    };
}

pub(crate) use ptr_metadata;

pub trait NonNull_<T> {
    fn cast_slice_(self, len: usize) -> NonNull<[T]>;
    fn cast_uninit_(self) -> NonNull<MaybeUninit<T>>;
}

pub trait NonNullSlice_<T> {
    fn as_mut_ptr_(self) -> *mut T;
}

pub trait MutPtr_<T> {
    fn cast_slice_(self, len: usize) -> *mut [T];
}

pub trait UnsafeCell_<T: ?Sized> {
    unsafe fn as_mut_unchecked_(&self) -> &mut T;
    unsafe fn as_ref_unchecked_(&self) -> &T;
    unsafe fn replace_(&self, value: T) -> T
        where T: Sized;
}

impl<T> NonNull_<T> for NonNull<T> {
    #[inline(always)]
    fn cast_slice_(self, len: usize) -> NonNull<[T]> {
        NonNull::slice_from_raw_parts(self, len)
    }

    #[inline(always)]
    fn cast_uninit_(self) -> NonNull<MaybeUninit<T>> {
        self.cast()
    }
}

impl<T> NonNullSlice_<T> for NonNull<[T]> {
    #[inline(always)]
    fn as_mut_ptr_(self) -> *mut T {
        self.cast().as_ptr()
    }
}

impl<T> MutPtr_<T> for *mut T {
    #[inline(always)]
    fn cast_slice_(self, len: usize) -> *mut [T] {
        ptr::slice_from_raw_parts_mut(self, len)
    }
}

impl<T: ?Sized> UnsafeCell_<T> for std::cell::UnsafeCell<T> {
    #[inline(always)]
    unsafe fn as_mut_unchecked_(&self) -> &mut T {
        // SAFETY: pointer comes from `&self` so naturally satisfies ptr-to-ref invariants.
        // SAFETY: the caller must guarantee that `self` is valid for a reference
        unsafe { &mut *self.get() }
    }

    #[inline(always)]
    unsafe fn as_ref_unchecked_(&self) -> &T {
        // SAFETY: pointer comes from `&self` so naturally satisfies ptr-to-ref invariants.
        // SAFETY: the caller must guarantee that `self` is valid for a reference
        unsafe { & *self.get() }
    }

    #[inline(always)]
    unsafe fn replace_(&self, value: T) -> T
        where T: Sized {
        // SAFETY: pointer comes from `&self` so naturally satisfies invariants.
        unsafe { ptr::replace(self.get(), value) }
    }
}