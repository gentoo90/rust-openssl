use libc::{c_char, c_int, c_long, c_void, strlen};
use ffi::{BIO, BIO_METHOD, BIO_CTRL_FLUSH, BIO_TYPE_NONE, BIO_new};
use ffi_extras::{BIO_clear_retry_flags, BIO_set_retry_read, BIO_set_retry_write};
use std::io;
use std::io::prelude::*;
use std::mem;
use std::slice;
use std::ptr;

use ssl::error::SslError;

// "rust"
const NAME: [c_char; 5] = [114, 117, 115, 116, 0];

// we use this after removing the stream from the BIO so that we don't have to
// worry about freeing the heap allocated BIO_METHOD after freeing the BIO.
static DESTROY_METHOD: BIO_METHOD = BIO_METHOD {
    type_: BIO_TYPE_NONE,
    name: &NAME[0],
    bwrite: None,
    bread: None,
    bputs: None,
    bgets: None,
    ctrl: None,
    create: None,
    destroy: Some(destroy),
    callback_ctrl: None,
};

pub struct StreamState<S> {
    pub stream: S,
    pub error: Option<io::Error>,
}

pub fn new<S: Read + Write>(stream: S) -> Result<(*mut BIO, Box<BIO_METHOD>), SslError> {

    let method = Box::new(BIO_METHOD {
        type_: BIO_TYPE_NONE,
        name: &NAME[0],
        bwrite: Some(bwrite::<S>),
        bread: Some(bread::<S>),
        bputs: Some(bputs::<S>),
        bgets: None,
        ctrl: Some(ctrl::<S>),
        create: Some(create),
        destroy: None, // covered in the replacement BIO_METHOD
        callback_ctrl: None,
    });

    let state = Box::new(StreamState {
        stream: stream,
        error: None,
    });

    unsafe {
        let bio = try_ssl_null!(BIO_new(&*method));
        (*bio).ptr = Box::into_raw(state) as *mut _;
        (*bio).init = 1;

        return Ok((bio, method));
    }
}

pub unsafe fn take_error<S>(bio: *mut BIO) -> Option<io::Error> {
    let state = state::<S>(bio);
    state.error.take()
}

pub unsafe fn take_stream<S>(bio: *mut BIO) -> S {
    let state: Box<StreamState<S>> = Box::from_raw((*bio).ptr as *mut _);
    (*bio).ptr = ptr::null_mut();
    (*bio).method = &DESTROY_METHOD as *const _ as *mut _;
    (*bio).init = 0;
    state.stream
}

pub unsafe fn get_ref<'a, S: 'a>(bio: *mut BIO) -> &'a S {
    let state: &'a StreamState<S> = mem::transmute((*bio).ptr);
    &state.stream
}

pub unsafe fn get_mut<'a, S: 'a>(bio: *mut BIO) -> &'a mut S {
    &mut state(bio).stream
}

unsafe fn state<'a, S: 'a>(bio: *mut BIO) -> &'a mut StreamState<S> {
    mem::transmute((*bio).ptr)
}

unsafe extern "C" fn bwrite<S: Write>(bio: *mut BIO, buf: *const c_char, len: c_int) -> c_int {
    BIO_clear_retry_flags(bio);

    let state = state::<S>(bio);
    let buf = slice::from_raw_parts(buf as *const _, len as usize);
    match state.stream.write(buf) {
        Ok(len) => len as c_int,
        Err(err) => {
            if retriable_error(&err) {
                BIO_set_retry_write(bio);
            }
            state.error = Some(err);
            -1
        }
    }
}

unsafe extern "C" fn bread<S: Read>(bio: *mut BIO, buf: *mut c_char, len: c_int) -> c_int {
    BIO_clear_retry_flags(bio);

    let state = state::<S>(bio);
    let buf = slice::from_raw_parts_mut(buf as *mut _, len as usize);
    match state.stream.read(buf) {
        Ok(len) => len as c_int,
        Err(err) => {
            if retriable_error(&err) {
                BIO_set_retry_read(bio);
            }
            state.error = Some(err);
            -1
        }
    }
}

fn retriable_error(err: &io::Error) -> bool {
    match err.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::NotConnected => true,
        _ => false
    }
}

unsafe extern "C" fn bputs<S: Write>(bio: *mut BIO, s: *const c_char) -> c_int {
    bwrite::<S>(bio, s, strlen(s) as c_int)
}

unsafe extern "C" fn ctrl<S: Write>(bio: *mut BIO,
                                    cmd: c_int,
                                    _num: c_long,
                                    _ptr: *mut c_void)
                                    -> c_long {
    if cmd == BIO_CTRL_FLUSH {
        let state = state::<S>(bio);
        match state.stream.flush() {
            Ok(()) => 1,
            Err(err) => {
                state.error = Some(err);
                0
            }
        }
    } else {
        0
    }
}

unsafe extern "C" fn create(bio: *mut BIO) -> c_int {
    (*bio).init = 0;
    (*bio).num = 0;
    (*bio).ptr = ptr::null_mut();
    (*bio).flags = 0;
    1
}

unsafe extern "C" fn destroy(bio: *mut BIO) -> c_int {
    if bio.is_null() {
        return 0;
    }

    assert!((*bio).ptr.is_null());
    1
}
