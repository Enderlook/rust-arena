use std::{any::Any, borrow::{Borrow, BorrowMut}, fmt, marker::PhantomData, mem::{ManuallyDrop, MaybeUninit}, ops::{Deref, DerefMut}, ptr::NonNull};

use crate::compatibility::*;

/// An owned pointer to an arena-allocated `T` value, that runs `Drop` implementations.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(transparent)]
pub struct Box<'a, T: ?Sized>(NonNull<T>, PhantomData<&'a ()>);

impl<'a, T> Box<'a, T> {
    /// Consumes the `Box<'a, T>`, returning the wrapped value.
    #[inline(always)]
    pub fn into_inner(this: Self) -> T {
        unsafe { core::ptr::read(Box::into_raw(this)) }
    }

    /// Converts `Box<T>` into a `Box<[T]>` of a single element.
    #[inline(always)]
    pub fn into_boxed_slice(this: Self) -> Box<'a, [T]> {
        let raw = Box::into_raw(this);
        unsafe { Box::from_raw(raw as *mut [T; 1]) }
    }

    /// Consumes the `Box<'a, T>` without loosing the allocation,
    /// returning the wrapped value and a `Box<'a, T>` to the uninitialized memory where the wrapped value used to live.
    pub fn take(this: Self) -> (T, Box<'a, MaybeUninit<T>>) {
        let raw = Box::into_non_null(this);
        let value = unsafe { raw.read() };
        let uninit = unsafe { Box::from_non_null(raw.cast_uninit_()) };
        (value, uninit)
    }
}

impl<'a, T: ?Sized> Box<'a, T> {
    /// Creates a box from a raw pointer, taking ownership of it.
    #[inline(always)]
    pub unsafe fn from_raw(raw: *mut T) -> Self {
        Box(unsafe { NonNull::new_unchecked(raw) }, PhantomData)
    }

    /// Creates a box from a non null pointer, taking ownership of it.
    #[inline(always)]
    pub unsafe fn from_non_null(ptr: NonNull<T>) -> Self {
        Box(ptr, PhantomData)
    }

    /// Returns the raw mutable pointer to the Box's content.
    #[inline(always)]
    pub fn as_mut_ptr(this: &mut Self) -> *mut T {
        this.0.as_ptr()
    }

    /// Returns the raw pointer to the Box's content.
    #[inline(always)]
    pub fn as_ptr(this: &Self) -> *const T {
        this.0.as_ptr()
    }

    /// Consumes the `Box<'a, T>`, returning a raw pointer whose ownership now belongs to the caller.
    #[must_use]
    #[inline(always)]
    pub fn into_raw(this: Self) -> *mut T {
        let mut b = ManuallyDrop::new(this);
        b.deref_mut().0.as_ptr()
    }

    /// Consumes the `Box<'a, T>`, returning a non null pointer whose ownership now belongs to the caller.
    #[inline(always)]
    #[must_use]
    pub fn into_non_null(this: Self) -> NonNull<T> {
        ManuallyDrop::new(this).deref_mut().0
    }
}

impl<'a, T> Box<'a, MaybeUninit<T>> {
    /// Converts to `Box<'a, T>`.
    pub unsafe fn assume_init(self) -> Box<'a, T> {
        let raw = Box::into_raw(self);
        unsafe { Box::from_raw(raw as *mut T) }
    }

    /// Writes the value and converts to `Box<'a, T>`.
    pub fn write(mut this: Self, value: T) -> Box<'a, T> {
        (*this).write(value);
        unsafe { this.assume_init() }
    }
}

impl<'a, T> Box<'a, [MaybeUninit<T>]> {
    /// Converts to `Box<'a, T>`.
    pub unsafe fn assume_init(self) -> Box<'a, [T]> {
        let raw = Box::into_raw(self);
        unsafe { Box::from_raw(raw as *mut [T]) }
    }
}

impl<'a, T> Box<'a, [T]> {
    /// Attempts to convert a boxed slice into a boxed array without any reallocation.
    /// This only succeed if the length of the slice matches with `N`.
    #[inline(always)]
    pub fn into_array<const N: usize>(self) -> Result<Box<'a, [T; N]>, Box<'a, [T]>> {
        if self.len() == N {
            let raw = Box::into_raw(self) as *mut [T; N];
            // This is safe because the layout are the same when the length match.
            Ok(unsafe { Box::from_raw(raw) })
        } else {
            Err(self)
        }
    }
}

impl<'a> Box<'a, dyn Any> {
    /// Attempts to downcast the box to a concrete type.
    #[inline]
    pub fn downcast<T: Any>(self) -> Result<Box<'a, T>, Self> {
        if self.is::<T>() {
            Ok(unsafe { self.downcast_unchecked() })
        } else {
            Err(self)
        }
    }

    /// Downcast the box to a concrete type.
    #[inline]
    pub unsafe fn downcast_unchecked<T: Any>(self) -> Box<'a, T> {
        debug_assert!(self.is::<T>());
        let raw = Box::into_raw(self);
        unsafe { Box::from_raw(raw as *mut T) }
    }
}

impl<'a> Box<'a, dyn Any + Send> {
    /// Attempts to downcast the box to a concrete type.
    #[inline]
    pub fn downcast<T: Any>(self) -> Result<Box<'a, T>, Self> {
        if self.is::<T>() {
            Ok(unsafe { self.downcast_unchecked() })
        } else {
            Err(self)
        }
    }

    /// Downcast the box to a concrete type.
    #[inline]
    pub unsafe fn downcast_unchecked<T: Any>(self) -> Box<'a, T> {
        debug_assert!(self.is::<T>());
        let raw = Box::into_raw(self);
        unsafe { Box::from_raw(raw as *mut T) }
    }
}

impl<'a> Box<'a, dyn Any + Send + Sync> {
    /// Attempts to downcast the box to a concrete type.
    #[inline]
    pub fn downcast<T: Any>(self) -> Result<Box<'a, T>, Self> {
        if self.is::<T>() {
            Ok(unsafe { self.downcast_unchecked() })
        } else {
            Err(self)
        }
    }

    /// Downcast the box to a concrete type.
    #[inline]
    pub unsafe fn downcast_unchecked<T: Any>(self) -> Box<'a, T> {
        debug_assert!(self.is::<T>());
        let raw = Box::into_raw(self);
        unsafe { Box::from_raw(raw as *mut T) }
    }
}

impl<'a, T: ?Sized> Drop for Box<'a, T> {
    #[inline(always)]
    fn drop(&mut self) {
        unsafe { core::ptr::drop_in_place(self.0.as_ptr()); }
    }
}

impl<'a, T> Default for Box<'a, [T]> {
    #[inline(always)]
    fn default() -> Box<'a, [T]> {
        // It's safe to drop an empty slice later.
        Box(NonNull::dangling().cast_slice_(0), PhantomData)
    }
}

impl<'a> Default for Box<'a, str> {
    #[inline(always)]
    fn default() -> Box<'a, str> {
        // It's safe to drop an empty string later.
        unsafe { Box::from_raw(Box::into_raw(Box::<[u8]>::default()) as *mut str) }
    }
}

impl<'a, T: ?Sized> Borrow<T> for Box<'a, T> {
    #[inline(always)]
    fn borrow(&self) -> &T {
        unsafe { self.0.as_ref() }
    }
}

impl<'a, T: ?Sized> BorrowMut<T> for Box<'a, T> {
    #[inline(always)]
    fn borrow_mut(&mut self) -> &mut T {
        unsafe { self.0.as_mut() }
    }
}

impl<'a, T: ?Sized> Deref for Box<'a, T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}

impl<'a, T: ?Sized> DerefMut for Box<'a, T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.as_mut() }
    }
}

impl<'a, T: ?Sized> AsRef<T> for Box<'a, T> {

    #[inline(always)]
    fn as_ref(&self) -> &T {
        unsafe { self.0.as_ref() }
    }
}

impl<'a, T: ?Sized> AsMut<T> for Box<'a, T> {

    #[inline(always)]
    fn as_mut(&mut self) -> &mut T {
        unsafe { self.0.as_mut() }
    }
}

impl<'a, T: fmt::Display + ?Sized> fmt::Display for Box<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

impl<'a, T: ?Sized> fmt::Pointer for Box<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Pointer::fmt(&(&**self as *const T), f)
    }
}