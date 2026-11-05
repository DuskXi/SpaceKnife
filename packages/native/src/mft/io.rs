//! Several large reads of the $MFT in flight at once (docs/native.md 2.1).
//!
//! An [`Engine`] takes positioned reads and hands their completions back in any order:
//! - Windows: [`Overlapped`], one handle opened for overlapped I/O with its completions on an I/O
//!   completion port (the volume handle is also unbuffered: offsets, lengths and buffer addresses
//!   are then sector aligned, which [`Buf`] and the window plan guarantee);
//! - everywhere (image files in tests): [`Threads`], one blocking reader thread per read in flight.
//!
//! A read's destination must stay valid until its completion has been returned by
//! [`Engine::wait`]; dropping an engine first waits for every read still in flight.

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::io;
use std::sync::{Arc, mpsc};

use super::runs::ReadAt;

pub const ALIGN: usize = 4096; // buffer alignment for unbuffered (sector-aligned) I/O

/// A zero-initialised heap buffer aligned to [`ALIGN`].
pub struct Buf {
    ptr: *mut u8,
    cap: usize,
}

// SAFETY: a Buf owns its memory exclusively, like a Vec<u8>.
unsafe impl Send for Buf {}
unsafe impl Sync for Buf {}

impl Buf {
    pub fn new(cap: usize) -> io::Result<Buf> {
        let cap = cap.max(1).next_multiple_of(ALIGN);
        let layout = Layout::from_size_align(cap, ALIGN)
            .map_err(|_| io::Error::new(io::ErrorKind::OutOfMemory, "buffer too large"))?;
        // SAFETY: the layout has a non-zero size.
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                format!("cannot allocate a {cap}-byte read buffer"),
            ));
        }
        Ok(Buf { ptr, cap })
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr
    }

    pub fn slice(&self, len: usize) -> &[u8] {
        assert!(len <= self.cap);
        // SAFETY: the allocation holds `cap` initialised bytes.
        unsafe { std::slice::from_raw_parts(self.ptr, len) }
    }

    pub fn slice_mut(&mut self, len: usize) -> &mut [u8] {
        assert!(len <= self.cap);
        // SAFETY: as above, and `&mut self` makes the access exclusive.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, len) }
    }
}

impl Drop for Buf {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` with this layout.
        unsafe { dealloc(self.ptr, Layout::from_size_align_unchecked(self.cap, ALIGN)) }
    }
}

/// One positioned read: `len` bytes at device offset `at` into `dst`.
#[derive(Clone, Copy)]
pub struct Read {
    pub at: u64,
    pub dst: *mut u8,
    pub len: usize,
    pub tag: usize,
}

// SAFETY: `dst` points into a buffer the submitter keeps alive and untouched until the read's
// completion is returned (the Engine contract); the reader only writes through it.
unsafe impl Send for Read {}

pub trait Engine {
    /// Start a read (it completes later, through [`Engine::wait`]).
    fn submit(&mut self, read: Read) -> io::Result<()>;
    /// The next finished read: its tag and outcome. Blocks; `None` when nothing is in flight.
    fn wait(&mut self) -> Option<(usize, io::Result<()>)>;
    fn in_flight(&self) -> usize;
}

// --------------------------------------------------------------------------- threads

/// Blocking positioned reads on worker threads (one per read in flight), any [`ReadAt`] device
/// (on Windows image files use [`Overlapped`] too: there this engine runs in unit tests only).
#[cfg_attr(windows, allow(dead_code))]
pub struct Threads {
    todo: Option<mpsc::Sender<Read>>,
    done: mpsc::Receiver<(usize, io::Result<()>)>,
    workers: Vec<std::thread::JoinHandle<()>>,
    live: usize,
}

#[cfg_attr(windows, allow(dead_code))]
impl Threads {
    pub fn new<R: ReadAt + Send + 'static>(dev: Arc<R>, threads: usize) -> io::Result<Threads> {
        let (todo, jobs) = mpsc::channel::<Read>();
        let (tell, done) = mpsc::channel();
        let jobs = Arc::new(std::sync::Mutex::new(jobs));
        let mut workers = Vec::new();
        for _ in 0..threads.max(1) {
            let (dev, jobs, tell) = (dev.clone(), jobs.clone(), tell.clone());
            workers.push(
                std::thread::Builder::new()
                    .name("sk-mft-read".into())
                    .spawn(move || {
                        loop {
                            let next = jobs.lock().unwrap().recv();
                            let Ok(r) = next else { break };
                            // SAFETY: the Engine contract keeps `dst` valid and exclusive.
                            let dst = unsafe { std::slice::from_raw_parts_mut(r.dst, r.len) };
                            let res = dev.read_exact_at(dst, r.at);
                            if tell.send((r.tag, res)).is_err() {
                                break;
                            }
                        }
                    })?,
            );
        }
        Ok(Threads {
            todo: Some(todo),
            done,
            workers,
            live: 0,
        })
    }
}

impl Engine for Threads {
    fn submit(&mut self, read: Read) -> io::Result<()> {
        let todo = self.todo.as_ref().expect("engine open");
        todo.send(read)
            .map_err(|_| io::Error::other("reader threads exited"))?;
        self.live += 1;
        Ok(())
    }

    fn wait(&mut self) -> Option<(usize, io::Result<()>)> {
        if self.live == 0 {
            return None;
        }
        let got = self.done.recv().ok()?;
        self.live -= 1;
        Some(got)
    }

    fn in_flight(&self) -> usize {
        self.live
    }
}

impl Drop for Threads {
    fn drop(&mut self) {
        while self.wait().is_some() {} // reads still writing into their buffers
        self.todo = None; // the workers see the channel close
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

// --------------------------------------------------------------------------- overlapped (Windows)

#[cfg(windows)]
pub use overlapped::Overlapped;

#[cfg(windows)]
mod overlapped {
    use std::fs::File;
    use std::io;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_HANDLE_EOF, ERROR_IO_PENDING, GetLastError, HANDLE,
    };
    use windows_sys::Win32::Storage::FileSystem::ReadFile;
    use windows_sys::Win32::System::IO::{
        CancelIoEx, CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED,
    };
    use windows_sys::Win32::System::Threading::INFINITE;

    use super::{Engine, Read};

    #[repr(C)]
    struct Slot {
        ov: OVERLAPPED, // first: the OVERLAPPED pointer a completion carries is the slot's
        read: Read,
        done: usize,
    }

    /// Reads on one handle opened with `FILE_FLAG_OVERLAPPED`, completed through an I/O
    /// completion port: as many in flight as are submitted, no thread per read.
    pub struct Overlapped {
        file: File,
        port: HANDLE,
        live: usize,
    }

    // SAFETY: the port and the handle may be used from any thread; the engine is used by one.
    unsafe impl Send for Overlapped {}

    fn last_error() -> io::Error {
        // SAFETY: plain FFI call.
        let code = unsafe { GetLastError() };
        if code == ERROR_HANDLE_EOF {
            return io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "read past the end of the volume",
            );
        }
        io::Error::from_raw_os_error(code as i32)
    }

    impl Overlapped {
        /// `file` must have been opened with `FILE_FLAG_OVERLAPPED`.
        pub fn new(file: File) -> io::Result<Overlapped> {
            // SAFETY: a valid handle; a new port (no existing one), key 0, one concurrent thread.
            let port = unsafe {
                CreateIoCompletionPort(file.as_raw_handle() as HANDLE, std::ptr::null_mut(), 0, 1)
            };
            if port.is_null() {
                return Err(last_error());
            }
            Ok(Overlapped {
                file,
                port,
                live: 0,
            })
        }

        /// Issue (or re-issue the rest of) the slot's read; on success the completion comes
        /// through the port, even when ReadFile finished at once.
        fn issue(&mut self, slot: Box<Slot>) -> io::Result<()> {
            let raw = Box::into_raw(slot);
            // SAFETY: `raw` stays allocated until its completion is dequeued (or reclaimed below
            // when ReadFile failed outright and nothing was queued).
            unsafe {
                let s = &mut *raw;
                let at = s.read.at + s.done as u64;
                s.ov = std::mem::zeroed();
                s.ov.Anonymous.Anonymous.Offset = at as u32;
                s.ov.Anonymous.Anonymous.OffsetHigh = (at >> 32) as u32;
                let len = (s.read.len - s.done).min(u32::MAX as usize & !4095) as u32;
                let ok = ReadFile(
                    self.file.as_raw_handle() as HANDLE,
                    s.read.dst.add(s.done),
                    len,
                    std::ptr::null_mut(),
                    &mut s.ov,
                );
                if ok == 0 && GetLastError() != ERROR_IO_PENDING {
                    let err = last_error();
                    drop(Box::from_raw(raw));
                    return Err(err);
                }
            }
            self.live += 1;
            Ok(())
        }
    }

    impl Engine for Overlapped {
        fn submit(&mut self, read: Read) -> io::Result<()> {
            // SAFETY: an all-zero OVERLAPPED is the documented initial state.
            let slot = Box::new(Slot {
                ov: unsafe { std::mem::zeroed() },
                read,
                done: 0,
            });
            self.issue(slot)
        }

        fn wait(&mut self) -> Option<(usize, io::Result<()>)> {
            loop {
                if self.live == 0 {
                    return None;
                }
                let mut bytes = 0u32;
                let mut key = 0usize;
                let mut ov: *mut OVERLAPPED = std::ptr::null_mut();
                // SAFETY: plain FFI call with valid out pointers.
                let ok = unsafe {
                    GetQueuedCompletionStatus(self.port, &mut bytes, &mut key, &mut ov, INFINITE)
                };
                if ov.is_null() {
                    // the port itself failed: nothing to attribute it to, give up on everything
                    let err = last_error();
                    self.live = 0;
                    return Some((usize::MAX, Err(err)));
                }
                self.live -= 1;
                // SAFETY: every queued OVERLAPPED is the first field of a Slot leaked by `issue`.
                let mut slot = unsafe { Box::from_raw(ov.cast::<Slot>()) };
                if ok == 0 {
                    return Some((slot.read.tag, Err(last_error())));
                }
                slot.done += bytes as usize;
                if slot.done >= slot.read.len {
                    return Some((slot.read.tag, Ok(())));
                }
                if bytes == 0 {
                    let err =
                        io::Error::new(io::ErrorKind::UnexpectedEof, "short read from the volume");
                    return Some((slot.read.tag, Err(err)));
                }
                let tag = slot.read.tag;
                if let Err(e) = self.issue(slot) {
                    return Some((tag, Err(e)));
                }
            }
        }

        fn in_flight(&self) -> usize {
            self.live
        }
    }

    impl Drop for Overlapped {
        fn drop(&mut self) {
            if self.live > 0 {
                // SAFETY: cancels this handle's outstanding reads; their completions still come.
                unsafe { CancelIoEx(self.file.as_raw_handle() as HANDLE, std::ptr::null()) };
                while self.wait().is_some() {}
            }
            // SAFETY: the port was created in `new` and is closed once.
            unsafe { CloseHandle(self.port) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Pattern;

    impl ReadAt for Pattern {
        fn read_exact_at(&self, buf: &mut [u8], off: u64) -> io::Result<()> {
            if off >= 1 << 40 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "past the end"));
            }
            for (i, b) in buf.iter_mut().enumerate() {
                *b = ((off + i as u64) % 251) as u8;
            }
            Ok(())
        }
    }

    #[test]
    fn threads_complete_every_read() {
        let mut eng = Threads::new(Arc::new(Pattern), 3).unwrap();
        let mut bufs: Vec<Buf> = (0..8).map(|_| Buf::new(10_000).unwrap()).collect();
        for (i, b) in bufs.iter_mut().enumerate() {
            let read = Read {
                at: i as u64 * 777,
                dst: b.as_mut_ptr(),
                len: 10_000,
                tag: i,
            };
            eng.submit(read).unwrap();
        }
        assert_eq!(eng.in_flight(), 8);
        let mut seen = Vec::new();
        while let Some((tag, res)) = eng.wait() {
            res.unwrap();
            seen.push(tag);
        }
        seen.sort_unstable();
        assert_eq!(seen, (0..8).collect::<Vec<_>>());
        for (i, b) in bufs.iter().enumerate() {
            let at = i as u64 * 777;
            assert!(
                b.slice(10_000)
                    .iter()
                    .enumerate()
                    .all(|(k, &v)| v == ((at + k as u64) % 251) as u8)
            );
        }
        let mut b = Buf::new(16).unwrap();
        eng.submit(Read {
            at: 1 << 41,
            dst: b.as_mut_ptr(),
            len: 16,
            tag: 99,
        })
        .unwrap();
        let (tag, res) = eng.wait().unwrap();
        assert_eq!(tag, 99);
        assert_eq!(res.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        assert!(eng.wait().is_none());
    }

    #[test]
    fn buffers_are_aligned_and_zeroed() {
        let b = Buf::new(5000).unwrap();
        assert_eq!(b.cap(), 8192);
        assert_eq!(b.slice(8192).as_ptr() as usize % ALIGN, 0);
        assert!(b.slice(8192).iter().all(|&v| v == 0));
    }
}
