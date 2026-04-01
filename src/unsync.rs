use std::{any::Any, borrow::Borrow, cell::Cell, fmt, marker::PhantomData, mem::{ManuallyDrop, MaybeUninit}, ops::{Deref, DerefMut}, ptr};

use crate::{AllocError, Arena, Box};

/// A simple reference-counted pointer to an arena-allocated `T` value, that runs `Drop` implementations.
/// This is "simple" because it only tracks strong references, not weak ones.
pub struct StrongRc<'a, T: ?Sized>(&'a StrongRcInner<T>, PhantomData<*const ()> /* Removes Send + Sync */);

struct StrongRcInner<T: ?Sized> {
    count: Cell<usize>,
    value: T,
}

impl<'a, T> StrongRc<'a, T> {
    /// Allocates a new `StrongRc<'a, T>` in the given arena and then places `value` into it.
    #[inline(always)]
    pub fn try_new_in(value: T, arena: &'a impl Arena) -> Result<Self, AllocError> {
        let value = Box::into_non_null(arena.try_alloc(StrongRcInner {
            count: Cell::new(1),
            value,
        })?);
        Ok(StrongRc(unsafe { value.as_ref() }, PhantomData))
    }

    /// Returns an instance of the `StrongRc<'a, T>` that is unique, uses a function to clone the value.
    #[inline(always)]
    pub fn into_unique_with(this: Self, arena: &'a impl Arena, with: impl FnOnce(&T) -> T) -> Result<Self, AllocError> {
        // Check if we are sharing.
        if this.0.count.get() != 1 {
            // Create new clone.
            Self::try_new_in(with(&this.0.value), arena)
            // `self` is dropped here, which decreases the count.
        } else {
            // Use current allocation.
            Ok(this)
        }
    }

    /// Converts `StrongRc<'a, T>` into a `StrongRc<'a, [T]>` of a single element.
    #[inline(always)]
    pub fn into_strong_rc_slice(this: Self) -> StrongRc<'a, [T]> {
        let raw = StrongRc::into_raw(this);
        unsafe { StrongRc::from_raw(raw as *mut StrongRcInner<[T; 1]>) }
    }
}

impl<'a, T: ?Sized> StrongRc<'a, T> {
    #[inline(always)]
    unsafe fn from_raw(raw: *mut StrongRcInner<T>) -> Self {
        StrongRc(unsafe { &mut *raw }, PhantomData)
    }

    #[inline(always)]
    fn into_raw(this: Self) -> *mut StrongRcInner<T> {
        let mut b = ManuallyDrop::new(this);
        (b.deref_mut().0 as *const StrongRcInner<T>) as *mut StrongRcInner<T>
    }

    /// Attempts to return a mutable reference to the value stored in the strong rc if this is the only instance of the rc.
    #[inline(always)]
    pub fn get_mut(this: &mut Self) -> Option<&mut T> {
        if Self::is_unique(this) {
            Some(unsafe { Self::get_mut_unchecked(this) })
        } else {
            None
        }
    }

    /// Returns a mutable reference to the value stored in the strong rc.
    #[inline(always)]
    pub unsafe fn get_mut_unchecked(this: &mut Self) -> &mut T {
        let ptr = this.0 as *const StrongRcInner<T> as *mut StrongRcInner<T>;
        unsafe { &mut (*ptr).value }
    }

    /// Determines if this reference is the only instance of the strong rc.
    #[inline(always)]
    pub fn is_unique(this: &Self) -> bool {
        this.0.count.get() == 1
    }

    /// Returns an instance of the strong rc that is unique.
    #[inline(always)]
    pub fn into_unique(this: Self, arena: &'a impl Arena) -> Result<Self, AllocError>
        where T: Clone {
        // Check if we are sharing.
        if Self::is_unique(&this) {
            // Use current allocation.
            Ok(this)
        } else {
            // Create new clone.
            Self::try_new_in(this.0.value.clone(), arena)
            // `self` is dropped here, which decreases the count.
        }
    }
}

impl<'a, T> StrongRc<'a, MaybeUninit<T>> {
    /// Converts to `StrongRc<'a, T>`.
    pub unsafe fn assume_init(self) -> StrongRc<'a, T> {
        let raw = StrongRc::into_raw(self);
        unsafe { StrongRc::from_raw(raw as *mut StrongRcInner<T>) }
    }
}

impl<'a, T> StrongRc<'a, [MaybeUninit<T>]> {
    /// Converts to `StrongRc<'a, T>`.
    pub unsafe fn assume_init(self) -> StrongRc<'a, [T]> {
        let raw = StrongRc::into_raw(self);
        unsafe { StrongRc::from_raw(raw as *mut StrongRcInner<[T]>) }
    }
}

impl<'a, T> StrongRc<'a, [T]> {
    /// Attempts to convert an strong rc slice into an strong rc array without any reallocation.
    /// This only succeed if the length of the slice matches with `N`.
    #[inline(always)]
    pub fn into_array<const N: usize>(self) -> Result<StrongRc<'a, [T; N]>, Self> {
        if self.len() == N {
            let raw = StrongRc::into_raw(self) as *mut StrongRcInner<[T; N]>;
            // This is safe because the layout are the same when the length match.
            Ok(unsafe { StrongRc::from_raw(raw) })
        } else {
            Err(self)
        }
    }
}

impl<'a> StrongRc<'a, dyn Any> {
    /// Attempts to downcast the `StrongRc<'a, T>` to a concrete type.
    #[inline]
    pub fn downcast<T: Any>(self) -> Result<StrongRc<'a, T>, Self> {
        if self.is::<T>() {
            Ok(unsafe { self.downcast_unchecked() })
        } else {
            Err(self)
        }
    }

    /// Downcast the `StrongRc<'a, T>` to a concrete type.
    #[inline]
    pub unsafe fn downcast_unchecked<T: Any>(self) -> StrongRc<'a, T> {
        debug_assert!(self.is::<T>());
        let raw = StrongRc::into_raw(self);
        unsafe { StrongRc::from_raw(raw as *mut StrongRcInner<T>) }
    }
}

impl<'a, T> Clone for StrongRc<'a, T> {
    #[inline(always)]
    fn clone(&self) -> Self {
        let count = self.0.count.get();
        self.0.count.set(count.checked_add(1).expect("references count overflow"));
        Self(self.0, PhantomData)
    }
}

impl<'a, T: ?Sized> Drop for StrongRc<'a, T> {
    #[inline(always)]
    fn drop(&mut self) {
        let count = self.0.count.get();
        if count == 1 {
            // Last reference, drop.
            unsafe { ptr::drop_in_place(&mut (&self.0.value as *const T as *mut T)); }
        } else {
            self.0.count.set(count - 1);
        }
    }
}


impl<'a, T: ?Sized> Borrow<T> for StrongRc<'a, T> {
    #[inline(always)]
    fn borrow(&self) -> &T {
        &self.0.value
    }
}

impl<'a, T: ?Sized> Deref for StrongRc<'a, T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.0.value
    }
}

impl<'a, T: ?Sized> AsRef<T> for StrongRc<'a, T> {

    #[inline(always)]
    fn as_ref(&self) -> &T {
        &self.0.value
    }
}

impl<'a, T: fmt::Display + ?Sized> fmt::Display for StrongRc<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

impl<'a, T: ?Sized> fmt::Pointer for StrongRc<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Pointer::fmt(&(&**self as *const T), f)
    }
}