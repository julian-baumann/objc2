use std::marker::ContravariantLifetime;
use std::mem;
use std::ptr;
use libc::c_ulong;

use objc::Id;
use objc::runtime::Object;

use INSObject;

pub struct NSEnumerator<'a, T> {
    id: Id<Object>,
    marker: ContravariantLifetime<'a>,
}

impl<'a, T> NSEnumerator<'a, T> where T: INSObject {
    pub unsafe fn from_ptr(ptr: *mut Object) -> NSEnumerator<'a, T> {
        NSEnumerator { id: Id::from_ptr(ptr), marker: ContravariantLifetime }
    }
}

impl<'a, T> Iterator for NSEnumerator<'a, T> where T: INSObject {
    type Item = &'a T;

    fn next(&mut self) -> Option<&'a T> {
        unsafe {
            let obj: *mut T = msg_send![self.id, nextObject];
            obj.as_ref()
        }
    }
}

pub trait INSFastEnumeration: INSObject {
    type Item: INSObject;

    fn enumerator(&self) -> NSFastEnumerator<Self> {
        NSFastEnumerator::new(self)
    }
}

#[repr(C)]
struct NSFastEnumerationState<T> {
    state: c_ulong,
    items_ptr: *const *const T,
    mutations_ptr: *mut c_ulong,
    extra: [c_ulong; 5],
}

const FAST_ENUM_BUF_SIZE: usize = 16;

pub struct NSFastEnumerator<'a, C: 'a + INSFastEnumeration> {
    object: &'a C,

    ptr: *const *const C::Item,
    end: *const *const C::Item,

    state: NSFastEnumerationState<C::Item>,
    buf: [*const C::Item; FAST_ENUM_BUF_SIZE],
}

impl<'a, C: INSFastEnumeration> NSFastEnumerator<'a, C> {
    fn new(object: &C) -> NSFastEnumerator<C> {
        NSFastEnumerator {
            object: object,

            ptr: ptr::null(),
            end: ptr::null(),

            state: unsafe { mem::zeroed() },
            buf: [ptr::null(); FAST_ENUM_BUF_SIZE],
        }
    }

    fn update_buf(&mut self) -> bool {
        // If this isn't our first time enumerating, record the previous value
        // from the mutations pointer.
        let mutations = if !self.ptr.is_null() {
            Some(unsafe { *self.state.mutations_ptr })
        } else {
            None
        };

        let count: usize = unsafe {
            msg_send![self.object, countByEnumeratingWithState:&mut self.state
                                                       objects:self.buf.as_mut_ptr()
                                                         count:self.buf.len()]
        };

        if count > 0 {
            // Check if the collection was mutated
            if let Some(mutations) = mutations {
                assert!(mutations == unsafe { *self.state.mutations_ptr },
                    "Mutation detected during enumeration of object {:?}",
                    self.object as *const C);
            }

            self.ptr = self.state.items_ptr;
            self.end = unsafe { self.ptr.offset(count as isize) };
            true
        } else {
            self.ptr = ptr::null();
            self.end = ptr::null();
            false
        }
    }
}

impl<'a, C: INSFastEnumeration> Iterator for NSFastEnumerator<'a, C> {
    type Item = &'a C::Item;

    fn next(&mut self) -> Option<&'a C::Item> {
        if self.ptr == self.end && !self.update_buf() {
            None
        } else {
            unsafe {
                let obj = *self.ptr;
                self.ptr = self.ptr.offset(1);
                Some(&*obj)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use objc::Id;
    use {INSArray, INSValue, NSArray, NSValue};
    use super::INSFastEnumeration;

    #[test]
    fn test_enumerator() {
        let vec: Vec<Id<NSValue<u32>>> = (0..4).map(INSValue::from_value).collect();
        let array: Id<NSArray<_>> = INSArray::from_vec(vec);

        let enumerator = array.object_enumerator();
        assert!(enumerator.count() == 4);

        let enumerator = array.object_enumerator();
        assert!(enumerator.enumerate().all(|(i, obj)| obj.value() == i as u32));
    }

    #[test]
    fn test_fast_enumerator() {
        let vec: Vec<Id<NSValue<u32>>> = (0..4).map(INSValue::from_value).collect();
        let array: Id<NSArray<_>> = INSArray::from_vec(vec);

        let enumerator = array.enumerator();
        assert!(enumerator.count() == 4);

        let enumerator = array.enumerator();
        assert!(enumerator.enumerate().all(|(i, obj)| obj.value() == i as u32));
    }
}
