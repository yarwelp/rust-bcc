extern crate libc;
use types::*;
use failure::Error;
use failure::ResultExt;
use bcc_sys::bccapi::*;
use std;
use std::io::Cursor;
use byteorder::{NativeEndian, WriteBytesExt};

use table::Table;

struct PerfCallback {
    raw_cb: Box<Fn(Vec<u8>)>,
}

const BPF_PERF_READER_PAGE_CNT: i32 = 64;

unsafe extern "C" fn raw_callback(pc: MutPointer, ptr: MutPointer, size: i32) {
    let slice = std::slice::from_raw_parts(ptr as *const u8, size as usize);
    let vec: Vec<u8> = slice.to_vec();
    (*(*(pc as *const PerfCallback)).raw_cb)(vec)
}

// need this to be represented in memory as just a pointer!!
// very important!!
#[repr(C)]
struct PerfReader {
    ptr: *mut perf_reader,
}

impl PerfReader {
    pub fn fd(&mut self) -> i32 {
        unsafe { perf_reader_fd(self.ptr) }
    }
}

impl Drop for PerfReader {
    fn drop(&mut self) {
        unsafe { perf_reader_free(self.ptr as MutPointer) }
    }
}

pub struct PerfMap {
    // table and callbacks are just in here to make sure the data stays owned/alive
    // TODO: improve this API
    table: Table,
    readers: Vec<PerfReader>,
    callbacks: Vec<Box<PerfCallback>>,
}

fn zero_vec(size: usize) -> Vec<u8> {
    let mut vec = Vec::with_capacity(size);
    for _ in 0..size {
        vec.push(0);
    }
    vec
}

pub fn init_perf_map<F: 'static>(mut table: Table, cb: F) -> Result<PerfMap, Error>
where
    F: Fn() -> Box<Fn(Vec<u8>)>,
{
    let fd = table.fd();
    let key_size = table.key_size();
    let leaf_size = table.leaf_size();
    let mut key = zero_vec(key_size);
    let leaf = zero_vec(leaf_size);

    if key_size != 4 || leaf_size != 4 {
        return Err(format_err!("passed table has wrong size"));
    }

    let mut readers: Vec<PerfReader> = vec![];
    let mut callbacks = vec![];
    let mut cur = Cursor::new(leaf);

    for cpu in 0..4 {
        // TODO: don't hardcode the CPU ids
        unsafe {
            let (mut reader, callback) = open_perf_buffer(cpu, cb())?;
            let perf_fd = reader.fd() as u32;
            readers.push(reader);
            callbacks.push(callback);

            cur.write_u32::<NativeEndian>(perf_fd)?;
            table.set(&mut key, &mut cur.get_mut()).context(
                "Unable to initialize perf map",
            )?;
            let r = bpf_get_next_key(
                fd,
                key.as_mut_ptr() as MutPointer,
                key.as_mut_ptr() as MutPointer,
            );
            if r != 0 {
                return Err(format_err!("todo: oh no"));
            }
            cur.set_position(0);
        }
    }
    Ok(PerfMap {
        table,
        readers,
        callbacks,
    })
}

impl PerfMap {
    pub fn poll(&mut self, timeout: i32) {
        unsafe {
            perf_reader_poll(
                self.readers.len() as i32,
                self.readers.as_ptr() as *mut *mut perf_reader,
                timeout,
            )
        };
    }
}

fn open_perf_buffer(
    cpu: i32,
    raw_cb: Box<Fn(Vec<u8>)>,
) -> Result<(PerfReader, Box<PerfCallback>), Error> {
    let mut callback = Box::new(PerfCallback { raw_cb: raw_cb });
    let reader = unsafe {
        bpf_open_perf_buffer(
            Some(
                (raw_callback) as
                    unsafe extern "C" fn(*mut std::os::raw::c_void, *mut std::os::raw::c_void, i32),
            ),
            None,
            callback.as_mut() as *mut _ as MutPointer,
            -1, /* pid */
            cpu,
            BPF_PERF_READER_PAGE_CNT,
        )
    };
    if reader == 0 as MutPointer {
        return Err(format_err!("failed to open perf buffer"));
    }
    Ok((PerfReader { ptr: reader as *mut perf_reader }, callback))
}
