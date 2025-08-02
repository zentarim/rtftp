use std::ffi::{CStr, CString};
use std::fmt::{Debug, Display, Formatter};
use std::str::FromStr;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::{ptr, slice};

#[cfg(test)]
mod tests;

#[allow(non_camel_case_types)]
#[repr(C)]
struct guestfs_h {
    _unused: [u8; 0],
}

const EOPT: isize = -1;

// See: include/guest_fs.h
const GUEST_FS_EVENT_APPLIANCE: u64 = 0x0010;

type GuestFSEventCallback = Option<
    unsafe extern "C" fn(
        g: *const guestfs_h,
        opaque: *const libc::c_void,
        event: u64,
        event_handle: libc::c_int,
        flags: libc::c_int,
        buf: *const libc::c_char,
        buf_len: libc::size_t,
        array: *const u64,
        array_len: libc::size_t,
    ),
>;

extern "C" fn guestfs_event_callback(
    _handle: *const guestfs_h,
    opaque: *const libc::c_void,
    _event: u64,
    _event_handle: libc::c_int,
    _flags: libc::c_int,
    buf: *const libc::c_char,
    buf_len: libc::size_t,
    _array: *const u64,
    _array_len: libc::size_t,
) {
    unsafe {
        let sender = &*(opaque as *mut Sender<Vec<u8>>);
        let appliance_error = slice::from_raw_parts(buf as *const u8, buf_len).to_vec();
        _ = sender.send(appliance_error);
    }
}

#[repr(C)]
#[allow(non_camel_case_types)]
struct guestfs_stat {
    dev: i64,
    ino: i64,
    mode: i64,
    nlink: i64,
    uid: i64,
    gid: i64,
    rdev: i64,
    size: i64,
    blksize: i64,
    blocks: i64,
    atime: i64,
    mtime: i64,
    ctime: i64,
}

impl Drop for guestfs_stat {
    fn drop(&mut self) {
        unsafe {
            guestfs_free_stat(self);
        }
    }
}

#[link(name = "guestfs")]
unsafe extern "C" {
    fn guestfs_create() -> *const guestfs_h;
    fn guestfs_close(handle: *const guestfs_h);
    fn guestfs_last_error(handle: *const guestfs_h) -> *const libc::c_char;
    fn guestfs_add_drive_opts(
        handle: *const guestfs_h,
        filename: *const libc::c_char,
        ...
    ) -> libc::c_int;

    fn guestfs_config(
        handle: *const guestfs_h,
        qemu_param: *const libc::c_char,
        qemu_value: *const libc::c_char,
    ) -> libc::c_int;

    fn guestfs_launch(handle: *const guestfs_h) -> libc::c_int;

    fn guestfs_list_partitions(handle: *const guestfs_h) -> *mut *mut libc::c_char;

    fn guestfs_mount_ro(
        handle: *const guestfs_h,
        device: *const libc::c_char,
        mountpoint: *const libc::c_char,
    ) -> libc::c_int;

    fn guestfs_set_append(
        handle: *const guestfs_h,
        append: *const libc::c_char,
        ...
    ) -> libc::c_int;

    fn guestfs_free_stat(guestfs_free_stat: *const guestfs_stat) -> libc::c_void;

    fn guestfs_stat(handle: *const guestfs_h, path: *const libc::c_char) -> *mut guestfs_stat;

    fn guestfs_set_pgroup(handle: *const guestfs_h, pgroup: libc::c_int) -> libc::c_int;

    fn guestfs_pread(
        handle: *const guestfs_h,
        path: *const libc::c_char,
        count: libc::c_int,
        offset: i64,
        size_r: *mut libc::size_t,
    ) -> *const libc::c_char;

    fn guestfs_set_event_callback(
        handle: *const guestfs_h,
        cb: GuestFSEventCallback,
        event_bitmask: u64,
        flags: libc::c_int,
        opaque: *const libc::c_void,
    ) -> libc::c_int;

    fn guestfs_set_error_handler(
        g: *const guestfs_h,
        guestfs_error_handler_cb: *const libc::c_void,
        opaque: *const libc::c_void,
    );

}

fn get_last_error(handle: *const guestfs_h) -> GuestFSError {
    unsafe {
        let error_message = guestfs_last_error(handle);
        if error_message.is_null() {
            GuestFSError::Unknown
        } else {
            GuestFSError::Generic(String::from(
                CStr::from_ptr(error_message).to_str().unwrap(),
            ))
        }
    }
}

fn disable_signals_propagation(handle: &*const guestfs_h) -> Result<(), GuestFSError> {
    if unsafe { guestfs_set_pgroup(*handle, 1) } == 0 {
        Ok(())
    } else {
        Err(get_last_error(*handle))
    }
}

pub(super) struct GuestFS {
    handle: *const guestfs_h,
    events_receiver: Receiver<Vec<u8>>,
    _events_sender: Box<Sender<Vec<u8>>>, // Ensure proper drop at the end of the structure's lifecycle.
}

impl GuestFS {
    pub(super) fn new() -> Self {
        let (sender, receiver) = channel::<Vec<u8>>();
        let c_ptr = Box::into_raw(Box::new(sender));
        let (handle, boxed_sender) = unsafe {
            let handle = guestfs_create();
            guestfs_set_error_handler(handle, ptr::null(), ptr::null());
            let set_callback_result = guestfs_set_event_callback(
                handle,
                Some(guestfs_event_callback),
                GUEST_FS_EVENT_APPLIANCE,
                0,
                c_ptr as *const libc::c_void,
            );
            if set_callback_result != 0 {
                let last_error = get_last_error(handle);
                panic!("GuestFS set event callback failed: {last_error}");
            }
            (handle, Box::from_raw(c_ptr))
        };
        if let Err(error) = disable_signals_propagation(&handle) {
            panic!("disable_signals_propagation failed: {error}");
        }
        Self {
            handle,
            events_receiver: receiver,
            _events_sender: boxed_sender,
        }
    }

    pub(super) fn add_disk<S: AsRef<str>>(
        &self,
        path: S,
        read_only: bool,
    ) -> Result<(), GuestFSError> {
        let ro_i32 = libc::c_int::from(if read_only { 1 } else { 0 });
        let read_only_opt = 0;
        let disk_path = CString::from_str(path.as_ref()).expect("CString::new failed");
        if unsafe {
            guestfs_add_drive_opts(self.handle, disk_path.as_ptr(), read_only_opt, ro_i32, EOPT)
        } == 0
        {
            Ok(())
        } else {
            match get_last_error(self.handle) {
                GuestFSError::Generic(message) => {
                    if message.contains("No such file or directory") {
                        Err(GuestFSError::DiskNotFound(message))
                    } else {
                        Err(GuestFSError::Generic(message))
                    }
                }
                other_error => Err(other_error),
            }
        }
    }

    pub(super) fn add_qemu_option(&self, key: &str, value: &str) -> Result<(), GuestFSError> {
        let c_str_key = CString::new(key).expect("CString::new failed");
        let c_str_value = CString::new(value).expect("CString::new failed");
        if unsafe { guestfs_config(self.handle, c_str_key.as_ptr(), c_str_value.as_ptr()) } == 0 {
            Ok(())
        } else {
            Err(get_last_error(self.handle))
        }
    }

    pub(super) fn launch(&self) -> Result<(), GuestFSError> {
        if unsafe { guestfs_launch(self.handle) } == 0 {
            Ok(())
        } else {
            Err(get_last_error(self.handle))
        }
    }

    pub(super) fn retrieve_appliance_stderr(&self) -> Vec<String> {
        self.events_receiver
            .try_iter()
            .map(|event| String::from_utf8_lossy(&event).to_string())
            .flat_map(|s| s.lines().map(|line| line.to_string()).collect::<Vec<_>>())
            .filter(|s| !s.is_empty())
            .collect::<Vec<String>>()
    }

    pub(super) fn list_partitions(&self) -> Result<Vec<String>, GuestFSError> {
        let result = unsafe { guestfs_list_partitions(self.handle) };
        if result.is_null() {
            return Err(get_last_error(self.handle));
        };
        let mut partitions_list: Vec<String> = Vec::new();
        for index in 0..100usize {
            let partition_name = unsafe {
                let entry_ptr = *result.add(index);
                if entry_ptr.is_null() {
                    break;
                }
                CString::from_raw(entry_ptr)
            };
            partitions_list.push(partition_name.into_string().unwrap());
        }
        unsafe { libc::free(result as *mut libc::c_void) };
        Ok(partitions_list)
    }

    pub(super) fn mount_ro(&self, device: &str, mountpoint: &str) -> Result<(), GuestFSError> {
        let c_str_device = CString::new(device).expect("CString::new failed");
        let c_str_mountpoint = CString::new(mountpoint).expect("CString::new failed");
        if unsafe {
            guestfs_mount_ro(
                self.handle,
                c_str_device.as_ptr(),
                c_str_mountpoint.as_ptr(),
            )
        } == 0
        {
            Ok(())
        } else {
            Err(get_last_error(self.handle))
        }
    }

    pub(super) fn get_size(&self, path: &str) -> Result<usize, GuestFSError> {
        let c_str_path = CString::new(path).expect("CString::new failed");
        let size = unsafe {
            let result = guestfs_stat(self.handle, c_str_path.as_ptr());
            if result.is_null() {
                return Err(get_last_error(self.handle));
            };
            let size = (*result).size;
            guestfs_free_stat(result);
            size
        };
        Ok(size as usize)
    }

    pub(super) fn set_append(&self, string: &str) -> Result<(), GuestFSError> {
        let c_str = CString::new(string).expect("CString::new failed");
        let result = unsafe { guestfs_set_append(self.handle, c_str.as_ptr()) };
        if result == 0 {
            Ok(())
        } else {
            Err(get_last_error(self.handle))
        }
    }

    pub(super) fn read_to(
        &self,
        path: &str,
        buffer: &mut [u8],
        offset: usize,
    ) -> Result<usize, GuestFSError> {
        let c_str_path = CString::new(path).expect("CString::new failed");
        unsafe {
            let mut size_r: libc::size_t = 0;
            let read_buffer = guestfs_pread(
                self.handle,
                c_str_path.as_ptr(),
                buffer.len() as libc::c_int,
                offset as i64,
                &mut size_r,
            );
            if read_buffer.is_null() {
                let last_error = get_last_error(self.handle);
                eprintln!("Can't read from {c_str_path:?}: {last_error}");
                Err(last_error)
            } else {
                ptr::copy_nonoverlapping(
                    read_buffer as *const u8,
                    buffer.as_mut_ptr(),
                    size_r as usize,
                );
                libc::free(read_buffer as *mut libc::c_void);
                Ok(size_r as usize)
            }
        }
    }
}

impl Drop for GuestFS {
    fn drop(&mut self) {
        unsafe { guestfs_close(self.handle) };
        _ = self.events_receiver.try_iter().collect::<Vec<_>>();
    }
}

impl Display for GuestFS {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write! {f, "<Handle: {:?}>", self.handle}
    }
}

impl Debug for GuestFS {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write! {f, "<Handle: {:?}>", self.handle}
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub(super) enum GuestFSError {
    Generic(String),
    DiskNotFound(String),
    ConnectionRefused(String),
    ShareNotFound(String),
    Unknown,
}

impl Display for GuestFSError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write! {f, "<GuestFSError: {self:?}>"}
    }
}
