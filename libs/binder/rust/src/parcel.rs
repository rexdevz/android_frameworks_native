/*
 * Copyright (C) 2020 The Android Open Source Project
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Container for messages that are sent via binder.

use crate::binder::AsNative;
use crate::error::{status_result, Result, StatusCode};
use crate::proxy::SpIBinder;
use crate::sys;

use std::cell::RefCell;
use std::convert::TryInto;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ptr;
use std::fmt;

mod file_descriptor;
mod parcelable;
mod parcelable_holder;

pub use self::file_descriptor::ParcelFileDescriptor;
pub use self::parcelable::{
    Deserialize, DeserializeArray, DeserializeOption, Serialize, SerializeArray, SerializeOption,
    Parcelable, NON_NULL_PARCELABLE_FLAG, NULL_PARCELABLE_FLAG,
};
pub use self::parcelable_holder::{ParcelableHolder, ParcelableMetadata};

/// Container for a message (data and object references) that can be sent
/// through Binder.
///
/// A Parcel can contain both serialized data that will be deserialized on the
/// other side of the IPC, and references to live Binder objects that will
/// result in the other side receiving a proxy Binder connected with the
/// original Binder in the Parcel.
pub enum Parcel {
    /// Owned parcel pointer
    Owned(*mut sys::AParcel),
    /// Borrowed parcel pointer (will not be destroyed on drop)
    Borrowed(*mut sys::AParcel),
}

/// A variant of Parcel that is known to be owned.
pub struct OwnedParcel {
    ptr: *mut sys::AParcel,
}

/// # Safety
///
/// This type guarantees that it owns the AParcel and that all access to
/// the AParcel happens through the OwnedParcel, so it is ok to send across
/// threads.
unsafe impl Send for OwnedParcel {}

/// A variant of Parcel that is known to be borrowed.
pub struct BorrowedParcel<'a> {
    inner: Parcel,
    _lifetime: PhantomData<&'a mut Parcel>,
}

impl OwnedParcel {
    /// Create a new empty `OwnedParcel`.
    pub fn new() -> OwnedParcel {
        let ptr = unsafe {
            // Safety: If `AParcel_create` succeeds, it always returns
            // a valid pointer. If it fails, the process will crash.
            sys::AParcel_create()
        };
        assert!(!ptr.is_null());
        Self { ptr }
    }

    /// Create an owned reference to a parcel object from a raw pointer.
    ///
    /// # Safety
    ///
    /// This constructor is safe if the raw pointer parameter is either null
    /// (resulting in `None`), or a valid pointer to an `AParcel` object. The
    /// parcel object must be owned by the caller prior to this call, as this
    /// constructor takes ownership of the parcel and will destroy it on drop.
    ///
    /// Additionally, the caller must guarantee that it is valid to take
    /// ownership of the AParcel object. All future access to the AParcel
    /// must happen through this `OwnedParcel`.
    ///
    /// Because `OwnedParcel` implements `Send`, the pointer must never point
    /// to any thread-local data, e.g., a variable on the stack, either directly
    /// or indirectly.
    pub unsafe fn from_raw(ptr: *mut sys::AParcel) -> Option<OwnedParcel> {
        ptr.as_mut().map(|ptr| Self { ptr })
    }

    /// Consume the parcel, transferring ownership to the caller.
    pub(crate) fn into_raw(self) -> *mut sys::AParcel {
        let ptr = self.ptr;
        let _ = ManuallyDrop::new(self);
        ptr
    }

    /// Convert this `OwnedParcel` into an owned `Parcel`.
    pub fn into_parcel(self) -> Parcel {
        Parcel::Owned(self.into_raw())
    }

    /// Get a borrowed view into the contents of this `Parcel`.
    pub fn borrowed(&mut self) -> BorrowedParcel<'_> {
        BorrowedParcel {
            inner: Parcel::Borrowed(self.ptr),
            _lifetime: PhantomData,
        }
    }
}

impl Default for OwnedParcel {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for OwnedParcel {
    fn clone(&self) -> Self {
        let mut new_parcel = Self::new();
        new_parcel
            .borrowed()
            .append_all_from(&Parcel::Borrowed(self.ptr))
            .expect("Failed to append from Parcel");
        new_parcel
    }
}

impl<'a> std::ops::Deref for BorrowedParcel<'a> {
    type Target = Parcel;
    fn deref(&self) -> &Parcel {
        &self.inner
    }
}
impl<'a> std::ops::DerefMut for BorrowedParcel<'a> {
    fn deref_mut(&mut self) -> &mut Parcel {
        &mut self.inner
    }
}

/// # Safety
///
/// The `Parcel` constructors guarantee that a `Parcel` object will always
/// contain a valid pointer to an `AParcel`.
unsafe impl AsNative<sys::AParcel> for Parcel {
    fn as_native(&self) -> *const sys::AParcel {
        match *self {
            Self::Owned(x) | Self::Borrowed(x) => x,
        }
    }

    fn as_native_mut(&mut self) -> *mut sys::AParcel {
        match *self {
            Self::Owned(x) | Self::Borrowed(x) => x,
        }
    }
}

impl Parcel {
    /// Create a new empty `Parcel`.
    ///
    /// Creates a new owned empty parcel that can be written to
    /// using the serialization methods and appended to and
    /// from using `append_from` and `append_from_all`.
    pub fn new() -> Parcel {
        let parcel = unsafe {
            // Safety: If `AParcel_create` succeeds, it always returns
            // a valid pointer. If it fails, the process will crash.
            sys::AParcel_create()
        };
        assert!(!parcel.is_null());
        Self::Owned(parcel)
    }

    /// Create a borrowed reference to a parcel object from a raw pointer.
    ///
    /// # Safety
    ///
    /// This constructor is safe if the raw pointer parameter is either null
    /// (resulting in `None`), or a valid pointer to an `AParcel` object.
    pub(crate) unsafe fn borrowed(ptr: *mut sys::AParcel) -> Option<Parcel> {
        ptr.as_mut().map(|ptr| Self::Borrowed(ptr))
    }
}

impl Default for Parcel {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for Parcel {
    fn clone(&self) -> Self {
        let mut new_parcel = Self::new();
        new_parcel
            .append_all_from(self)
            .expect("Failed to append from Parcel");
        new_parcel
    }
}

// Data serialization methods
impl Parcel {
    /// Data written to parcelable is zero'd before being deleted or reallocated.
    pub fn mark_sensitive(&mut self) {
        unsafe {
            // Safety: guaranteed to have a parcel object, and this method never fails
            sys::AParcel_markSensitive(self.as_native())
        }
    }

    /// Write a type that implements [`Serialize`] to the `Parcel`.
    pub fn write<S: Serialize + ?Sized>(&mut self, parcelable: &S) -> Result<()> {
        parcelable.serialize(self)
    }

    /// Writes the length of a slice to the `Parcel`.
    ///
    /// This is used in AIDL-generated client side code to indicate the
    /// allocated space for an output array parameter.
    pub fn write_slice_size<T>(&mut self, slice: Option<&[T]>) -> Result<()> {
        if let Some(slice) = slice {
            let len: i32 = slice.len().try_into().or(Err(StatusCode::BAD_VALUE))?;
            self.write(&len)
        } else {
            self.write(&-1i32)
        }
    }

    /// Perform a series of writes to the `Parcel`, prepended with the length
    /// (in bytes) of the written data.
    ///
    /// The length `0i32` will be written to the parcel first, followed by the
    /// writes performed by the callback. The initial length will then be
    /// updated to the length of all data written by the callback, plus the
    /// size of the length elemement itself (4 bytes).
    ///
    /// # Examples
    ///
    /// After the following call:
    ///
    /// ```
    /// # use binder::{Binder, Interface, Parcel};
    /// # let mut parcel = Parcel::Owned(std::ptr::null_mut());
    /// parcel.sized_write(|subparcel| {
    ///     subparcel.write(&1u32)?;
    ///     subparcel.write(&2u32)?;
    ///     subparcel.write(&3u32)
    /// });
    /// ```
    ///
    /// `parcel` will contain the following:
    ///
    /// ```ignore
    /// [16i32, 1u32, 2u32, 3u32]
    /// ```
    pub fn sized_write<F>(&mut self, f: F) -> Result<()>
    where for<'a>
        F: Fn(&'a WritableSubParcel<'a>) -> Result<()>
    {
        let start = self.get_data_position();
        self.write(&0i32)?;
        {
            let subparcel = WritableSubParcel(RefCell::new(self));
            f(&subparcel)?;
        }
        let end = self.get_data_position();
        unsafe {
            self.set_data_position(start)?;
        }
        assert!(end >= start);
        self.write(&(end - start))?;
        unsafe {
            self.set_data_position(end)?;
        }
        Ok(())
    }

    /// Returns the current position in the parcel data.
    pub fn get_data_position(&self) -> i32 {
        unsafe {
            // Safety: `Parcel` always contains a valid pointer to an `AParcel`,
            // and this call is otherwise safe.
            sys::AParcel_getDataPosition(self.as_native())
        }
    }

    /// Returns the total size of the parcel.
    pub fn get_data_size(&self) -> i32 {
        unsafe {
            // Safety: `Parcel` always contains a valid pointer to an `AParcel`,
            // and this call is otherwise safe.
            sys::AParcel_getDataSize(self.as_native())
        }
    }

    /// Move the current read/write position in the parcel.
    ///
    /// # Safety
    ///
    /// This method is safe if `pos` is less than the current size of the parcel
    /// data buffer. Otherwise, we are relying on correct bounds checking in the
    /// Parcel C++ code on every subsequent read or write to this parcel. If all
    /// accesses are bounds checked, this call is still safe, but we can't rely
    /// on that.
    pub unsafe fn set_data_position(&self, pos: i32) -> Result<()> {
        status_result(sys::AParcel_setDataPosition(self.as_native(), pos))
    }

    /// Append a subset of another `Parcel`.
    ///
    /// This appends `size` bytes of data from `other` starting at offset
    /// `start` to the current `Parcel`, or returns an error if not possible.
    pub fn append_from(&mut self, other: &Self, start: i32, size: i32) -> Result<()> {
        let status = unsafe {
            // Safety: `Parcel::appendFrom` from C++ checks that `start`
            // and `size` are in bounds, and returns an error otherwise.
            // Both `self` and `other` always contain valid pointers.
            sys::AParcel_appendFrom(
                other.as_native(),
                self.as_native_mut(),
                start,
                size,
            )
        };
        status_result(status)
    }

    /// Append the contents of another `Parcel`.
    pub fn append_all_from(&mut self, other: &Self) -> Result<()> {
        self.append_from(other, 0, other.get_data_size())
    }
}

/// A segment of a writable parcel, used for [`Parcel::sized_write`].
pub struct WritableSubParcel<'a>(RefCell<&'a mut Parcel>);

impl<'a> WritableSubParcel<'a> {
    /// Write a type that implements [`Serialize`] to the sub-parcel.
    pub fn write<S: Serialize + ?Sized>(&self, parcelable: &S) -> Result<()> {
        parcelable.serialize(&mut *self.0.borrow_mut())
    }
}

// Data deserialization methods
impl Parcel {
    /// Attempt to read a type that implements [`Deserialize`] from this
    /// `Parcel`.
    pub fn read<D: Deserialize>(&self) -> Result<D> {
        D::deserialize(self)
    }

    /// Attempt to read a type that implements [`Deserialize`] from this
    /// `Parcel` onto an existing value. This operation will overwrite the old
    /// value partially or completely, depending on how much data is available.
    pub fn read_onto<D: Deserialize>(&self, x: &mut D) -> Result<()> {
        x.deserialize_from(self)
    }

    /// Safely read a sized parcelable.
    ///
    /// Read the size of a parcelable, compute the end position
    /// of that parcelable, then build a sized readable sub-parcel
    /// and call a closure with the sub-parcel as its parameter.
    /// The closure can keep reading data from the sub-parcel
    /// until it runs out of input data. The closure is responsible
    /// for calling [`ReadableSubParcel::has_more_data`] to check for
    /// more data before every read, at least until Rust generators
    /// are stabilized.
    /// After the closure returns, skip to the end of the current
    /// parcelable regardless of how much the closure has read.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// let mut parcelable = Default::default();
    /// parcel.sized_read(|subparcel| {
    ///     if subparcel.has_more_data() {
    ///         parcelable.a = subparcel.read()?;
    ///     }
    ///     if subparcel.has_more_data() {
    ///         parcelable.b = subparcel.read()?;
    ///     }
    ///     Ok(())
    /// });
    /// ```
    ///
    pub fn sized_read<F>(&self, mut f: F) -> Result<()>
    where
        for<'a> F: FnMut(ReadableSubParcel<'a>) -> Result<()>
    {
        let start = self.get_data_position();
        let parcelable_size: i32 = self.read()?;
        if parcelable_size < 0 {
            return Err(StatusCode::BAD_VALUE);
        }

        let end = start.checked_add(parcelable_size)
            .ok_or(StatusCode::BAD_VALUE)?;
        if end > self.get_data_size() {
            return Err(StatusCode::NOT_ENOUGH_DATA);
        }

        let subparcel = ReadableSubParcel {
            parcel: self,
            end_position: end,
        };
        f(subparcel)?;

        // Advance the data position to the actual end,
        // in case the closure read less data than was available
        unsafe {
            self.set_data_position(end)?;
        }

        Ok(())
    }

    /// Read a vector size from the `Parcel` and resize the given output vector
    /// to be correctly sized for that amount of data.
    ///
    /// This method is used in AIDL-generated server side code for methods that
    /// take a mutable slice reference parameter.
    pub fn resize_out_vec<D: Default + Deserialize>(&self, out_vec: &mut Vec<D>) -> Result<()> {
        let len: i32 = self.read()?;

        if len < 0 {
            return Err(StatusCode::UNEXPECTED_NULL);
        }

        // usize in Rust may be 16-bit, so i32 may not fit
        let len = len.try_into().unwrap();
        out_vec.resize_with(len, Default::default);

        Ok(())
    }

    /// Read a vector size from the `Parcel` and either create a correctly sized
    /// vector for that amount of data or set the output parameter to None if
    /// the vector should be null.
    ///
    /// This method is used in AIDL-generated server side code for methods that
    /// take a mutable slice reference parameter.
    pub fn resize_nullable_out_vec<D: Default + Deserialize>(
        &self,
        out_vec: &mut Option<Vec<D>>,
    ) -> Result<()> {
        let len: i32 = self.read()?;

        if len < 0 {
            *out_vec = None;
        } else {
            // usize in Rust may be 16-bit, so i32 may not fit
            let len = len.try_into().unwrap();
            let mut vec = Vec::with_capacity(len);
            vec.resize_with(len, Default::default);
            *out_vec = Some(vec);
        }

        Ok(())
    }
}

/// A segment of a readable parcel, used for [`Parcel::sized_read`].
pub struct ReadableSubParcel<'a> {
    parcel: &'a Parcel,
    end_position: i32,
}

impl<'a> ReadableSubParcel<'a> {
    /// Read a type that implements [`Deserialize`] from the sub-parcel.
    pub fn read<D: Deserialize>(&self) -> Result<D> {
        // The caller should have checked this,
        // but it can't hurt to double-check
        assert!(self.has_more_data());
        D::deserialize(self.parcel)
    }

    /// Check if the sub-parcel has more data to read
    pub fn has_more_data(&self) -> bool {
        self.parcel.get_data_position() < self.end_position
    }
}

// Internal APIs
impl Parcel {
    pub(crate) fn write_binder(&mut self, binder: Option<&SpIBinder>) -> Result<()> {
        unsafe {
            // Safety: `Parcel` always contains a valid pointer to an
            // `AParcel`. `AsNative` for `Option<SpIBinder`> will either return
            // null or a valid pointer to an `AIBinder`, both of which are
            // valid, safe inputs to `AParcel_writeStrongBinder`.
            //
            // This call does not take ownership of the binder. However, it does
            // require a mutable pointer, which we cannot extract from an
            // immutable reference, so we clone the binder, incrementing the
            // refcount before the call. The refcount will be immediately
            // decremented when this temporary is dropped.
            status_result(sys::AParcel_writeStrongBinder(
                self.as_native_mut(),
                binder.cloned().as_native_mut(),
            ))
        }
    }

    pub(crate) fn read_binder(&self) -> Result<Option<SpIBinder>> {
        let mut binder = ptr::null_mut();
        let status = unsafe {
            // Safety: `Parcel` always contains a valid pointer to an
            // `AParcel`. We pass a valid, mutable out pointer to the `binder`
            // parameter. After this call, `binder` will be either null or a
            // valid pointer to an `AIBinder` owned by the caller.
            sys::AParcel_readStrongBinder(self.as_native(), &mut binder)
        };

        status_result(status)?;

        Ok(unsafe {
            // Safety: `binder` is either null or a valid, owned pointer at this
            // point, so can be safely passed to `SpIBinder::from_raw`.
            SpIBinder::from_raw(binder)
        })
    }
}

impl Drop for Parcel {
    fn drop(&mut self) {
        // Run the C++ Parcel complete object destructor
        if let Self::Owned(ptr) = *self {
            unsafe {
                // Safety: `Parcel` always contains a valid pointer to an
                // `AParcel`. If we own the parcel, we can safely delete it
                // here.
                sys::AParcel_delete(ptr)
            }
        }
    }
}

impl Drop for OwnedParcel {
    fn drop(&mut self) {
        // Run the C++ Parcel complete object destructor
        unsafe {
            // Safety: `OwnedParcel` always contains a valid pointer to an
            // `AParcel`. Since we own the parcel, we can safely delete it
            // here.
            sys::AParcel_delete(self.ptr)
        }
    }
}

impl fmt::Debug for Parcel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Parcel")
            .finish()
    }
}

impl fmt::Debug for OwnedParcel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnedParcel")
            .finish()
    }
}

#[test]
fn test_read_write() {
    let mut parcel = Parcel::new();
    let start = parcel.get_data_position();

    assert_eq!(parcel.read::<bool>(), Err(StatusCode::NOT_ENOUGH_DATA));
    assert_eq!(parcel.read::<i8>(), Err(StatusCode::NOT_ENOUGH_DATA));
    assert_eq!(parcel.read::<u16>(), Err(StatusCode::NOT_ENOUGH_DATA));
    assert_eq!(parcel.read::<i32>(), Err(StatusCode::NOT_ENOUGH_DATA));
    assert_eq!(parcel.read::<u32>(), Err(StatusCode::NOT_ENOUGH_DATA));
    assert_eq!(parcel.read::<i64>(), Err(StatusCode::NOT_ENOUGH_DATA));
    assert_eq!(parcel.read::<u64>(), Err(StatusCode::NOT_ENOUGH_DATA));
    assert_eq!(parcel.read::<f32>(), Err(StatusCode::NOT_ENOUGH_DATA));
    assert_eq!(parcel.read::<f64>(), Err(StatusCode::NOT_ENOUGH_DATA));
    assert_eq!(parcel.read::<Option<String>>(), Ok(None));
    assert_eq!(parcel.read::<String>(), Err(StatusCode::UNEXPECTED_NULL));

    assert_eq!(parcel.read_binder().err(), Some(StatusCode::BAD_TYPE));

    parcel.write(&1i32).unwrap();

    unsafe {
        parcel.set_data_position(start).unwrap();
    }

    let i: i32 = parcel.read().unwrap();
    assert_eq!(i, 1i32);
}

#[test]
#[allow(clippy::float_cmp)]
fn test_read_data() {
    let mut parcel = Parcel::new();
    let str_start = parcel.get_data_position();

    parcel.write(&b"Hello, Binder!\0"[..]).unwrap();
    // Skip over string length
    unsafe {
        assert!(parcel.set_data_position(str_start).is_ok());
    }
    assert_eq!(parcel.read::<i32>().unwrap(), 15);
    let start = parcel.get_data_position();

    assert!(parcel.read::<bool>().unwrap());

    unsafe {
        assert!(parcel.set_data_position(start).is_ok());
    }

    assert_eq!(parcel.read::<i8>().unwrap(), 72i8);

    unsafe {
        assert!(parcel.set_data_position(start).is_ok());
    }

    assert_eq!(parcel.read::<u16>().unwrap(), 25928);

    unsafe {
        assert!(parcel.set_data_position(start).is_ok());
    }

    assert_eq!(parcel.read::<i32>().unwrap(), 1819043144);

    unsafe {
        assert!(parcel.set_data_position(start).is_ok());
    }

    assert_eq!(parcel.read::<u32>().unwrap(), 1819043144);

    unsafe {
        assert!(parcel.set_data_position(start).is_ok());
    }

    assert_eq!(parcel.read::<i64>().unwrap(), 4764857262830019912);

    unsafe {
        assert!(parcel.set_data_position(start).is_ok());
    }

    assert_eq!(parcel.read::<u64>().unwrap(), 4764857262830019912);

    unsafe {
        assert!(parcel.set_data_position(start).is_ok());
    }

    assert_eq!(
        parcel.read::<f32>().unwrap(),
        1143139100000000000000000000.0
    );
    assert_eq!(parcel.read::<f32>().unwrap(), 40.043392);

    unsafe {
        assert!(parcel.set_data_position(start).is_ok());
    }

    assert_eq!(parcel.read::<f64>().unwrap(), 34732488246.197815);

    // Skip back to before the string length
    unsafe {
        assert!(parcel.set_data_position(str_start).is_ok());
    }

    assert_eq!(parcel.read::<Vec<u8>>().unwrap(), b"Hello, Binder!\0");
}

#[test]
fn test_utf8_utf16_conversions() {
    let mut parcel = Parcel::new();
    let start = parcel.get_data_position();

    assert!(parcel.write("Hello, Binder!").is_ok());
    unsafe {
        assert!(parcel.set_data_position(start).is_ok());
    }
    assert_eq!(
        parcel.read::<Option<String>>().unwrap().unwrap(),
        "Hello, Binder!",
    );
    unsafe {
        assert!(parcel.set_data_position(start).is_ok());
    }

    assert!(parcel.write("Embedded null \0 inside a string").is_ok());
    unsafe {
        assert!(parcel.set_data_position(start).is_ok());
    }
    assert_eq!(
        parcel.read::<Option<String>>().unwrap().unwrap(),
        "Embedded null \0 inside a string",
    );
    unsafe {
        assert!(parcel.set_data_position(start).is_ok());
    }

    assert!(parcel.write(&["str1", "str2", "str3"][..]).is_ok());
    assert!(parcel
        .write(
            &[
                String::from("str4"),
                String::from("str5"),
                String::from("str6"),
            ][..]
        )
        .is_ok());

    let s1 = "Hello, Binder!";
    let s2 = "This is a utf8 string.";
    let s3 = "Some more text here.";

    assert!(parcel.write(&[s1, s2, s3][..]).is_ok());
    unsafe {
        assert!(parcel.set_data_position(start).is_ok());
    }

    assert_eq!(
        parcel.read::<Vec<String>>().unwrap(),
        ["str1", "str2", "str3"]
    );
    assert_eq!(
        parcel.read::<Vec<String>>().unwrap(),
        ["str4", "str5", "str6"]
    );
    assert_eq!(parcel.read::<Vec<String>>().unwrap(), [s1, s2, s3]);
}

#[test]
fn test_sized_write() {
    let mut parcel = Parcel::new();
    let start = parcel.get_data_position();

    let arr = [1i32, 2i32, 3i32];

    parcel.sized_write(|subparcel| {
        subparcel.write(&arr[..])
    }).expect("Could not perform sized write");

    // i32 sub-parcel length + i32 array length + 3 i32 elements
    let expected_len = 20i32;

    assert_eq!(parcel.get_data_position(), start + expected_len);

    unsafe {
        parcel.set_data_position(start).unwrap();
    }

    assert_eq!(
        expected_len,
        parcel.read().unwrap(),
    );

    assert_eq!(
        parcel.read::<Vec<i32>>().unwrap(),
        &arr,
    );
}

#[test]
fn test_append_from() {
    let mut parcel1 = Parcel::new();
    parcel1.write(&42i32).expect("Could not perform write");

    let mut parcel2 = Parcel::new();
    assert_eq!(Ok(()), parcel2.append_all_from(&parcel1));
    assert_eq!(4, parcel2.get_data_size());
    assert_eq!(Ok(()), parcel2.append_all_from(&parcel1));
    assert_eq!(8, parcel2.get_data_size());
    unsafe {
        parcel2.set_data_position(0).unwrap();
    }
    assert_eq!(Ok(42), parcel2.read::<i32>());
    assert_eq!(Ok(42), parcel2.read::<i32>());

    let mut parcel2 = Parcel::new();
    assert_eq!(Ok(()), parcel2.append_from(&parcel1, 0, 2));
    assert_eq!(Ok(()), parcel2.append_from(&parcel1, 2, 2));
    assert_eq!(4, parcel2.get_data_size());
    unsafe {
        parcel2.set_data_position(0).unwrap();
    }
    assert_eq!(Ok(42), parcel2.read::<i32>());

    let mut parcel2 = Parcel::new();
    assert_eq!(Ok(()), parcel2.append_from(&parcel1, 0, 2));
    assert_eq!(2, parcel2.get_data_size());
    unsafe {
        parcel2.set_data_position(0).unwrap();
    }
    assert_eq!(Err(StatusCode::NOT_ENOUGH_DATA), parcel2.read::<i32>());

    let mut parcel2 = Parcel::new();
    assert_eq!(Err(StatusCode::BAD_VALUE), parcel2.append_from(&parcel1, 4, 2));
    assert_eq!(Err(StatusCode::BAD_VALUE), parcel2.append_from(&parcel1, 2, 4));
    assert_eq!(Err(StatusCode::BAD_VALUE), parcel2.append_from(&parcel1, -1, 4));
    assert_eq!(Err(StatusCode::BAD_VALUE), parcel2.append_from(&parcel1, 2, -1));
}