//! Cross-platform transport for the daemon's JSON-lines protocol.
//!
//! Unix listens on a socket file (mode 0600), Windows on a per-user named
//! pipe (`\\.\pipe\aether-<user>`; the default pipe DACL only grants the
//! creating user). Either way the wire format is one JSON value per line,
//! so everything above this layer is platform independent.
//!
//! The Windows side binds kernel32 directly instead of pulling in a
//! windows-sys feature tree: the pipe API needs six functions.

use std::io::{self, Read, Write};
use std::path::Path;

#[cfg(unix)]
use std::net::Shutdown;
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};

#[cfg(not(any(unix, windows)))]
compile_error!("aether-daemon needs Unix sockets or Windows named pipes");

/// Listening endpoint: a socket file on Unix, a named pipe on Windows.
pub struct Listener {
    inner: ListenerInner,
}

enum ListenerInner {
    #[cfg(unix)]
    Unix(UnixListener),
    #[cfg(windows)]
    Pipe(PipeListener),
}

impl Listener {
    pub fn bind(path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            UnixListener::bind(path).map(|listener| Self {
                inner: ListenerInner::Unix(listener),
            })
        }
        #[cfg(windows)]
        {
            PipeListener::bind(path).map(|listener| Self {
                inner: ListenerInner::Pipe(listener),
            })
        }
    }

    /// Block until a client connects.
    pub fn accept(&mut self) -> io::Result<Conn> {
        match &mut self.inner {
            #[cfg(unix)]
            ListenerInner::Unix(listener) => listener.accept().map(|(stream, _)| Conn {
                inner: ConnInner::Unix(stream),
            }),
            #[cfg(windows)]
            ListenerInner::Pipe(listener) => listener.accept().map(|conn| Conn {
                inner: ConnInner::Pipe(conn),
            }),
        }
    }
}

/// One connected client: a bidirectional JSON-lines stream.
pub struct Conn {
    inner: ConnInner,
}

enum ConnInner {
    #[cfg(unix)]
    Unix(UnixStream),
    #[cfg(windows)]
    Pipe(PipeConn),
}

impl Conn {
    pub fn connect(path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            UnixStream::connect(path).map(|stream| Self {
                inner: ConnInner::Unix(stream),
            })
        }
        #[cfg(windows)]
        {
            PipeConn::connect(path).map(|conn| Self {
                inner: ConnInner::Pipe(conn),
            })
        }
    }

    /// Independent handle to the same connection (one for reading, one for
    /// writing). Unix duplicates the fd, Windows duplicates the handle.
    pub fn try_clone(&self) -> io::Result<Self> {
        match &self.inner {
            #[cfg(unix)]
            ConnInner::Unix(stream) => stream.try_clone().map(|stream| Self {
                inner: ConnInner::Unix(stream),
            }),
            #[cfg(windows)]
            ConnInner::Pipe(conn) => conn.duplicate().map(|conn| Self {
                inner: ConnInner::Pipe(conn),
            }),
        }
    }

    /// Close both directions; called when a client session ends.
    pub fn shutdown(&self) {
        match &self.inner {
            #[cfg(unix)]
            ConnInner::Unix(stream) => {
                let _ = stream.shutdown(Shutdown::Both);
            }
            #[cfg(windows)]
            ConnInner::Pipe(conn) => conn.disconnect(),
        }
    }
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.inner {
            #[cfg(unix)]
            ConnInner::Unix(stream) => stream.read(buf),
            #[cfg(windows)]
            ConnInner::Pipe(conn) => conn.read(buf),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match &mut self.inner {
            #[cfg(unix)]
            ConnInner::Unix(stream) => stream.write(buf),
            #[cfg(windows)]
            ConnInner::Pipe(conn) => conn.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.inner {
            #[cfg(unix)]
            ConnInner::Unix(stream) => stream.flush(),
            #[cfg(windows)]
            ConnInner::Pipe(conn) => conn.flush(),
        }
    }
}

// --------------------------------------------------------------------------
// Windows named pipe implementation
// --------------------------------------------------------------------------

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    // std::os::windows::raw no longer exports these on stable; both are the
    // documented Win32 shapes (HANDLE = pointer-sized, BOOL = 32-bit int).
    pub type HANDLE = *mut c_void;
    pub type BOOL = i32;

    #[link(name = "kernel32")]
    extern "system" {
        pub fn CreateNamedPipeW(
            name: *const u16,
            open_mode: u32,
            pipe_mode: u32,
            max_instances: u32,
            out_buffer_size: u32,
            in_buffer_size: u32,
            default_timeout_ms: u32,
            security_attributes: *mut c_void,
        ) -> HANDLE;
        pub fn ConnectNamedPipe(handle: HANDLE, overlapped: *mut c_void) -> BOOL;
        pub fn CreateFileW(
            name: *const u16,
            desired_access: u32,
            share_mode: u32,
            security_attributes: *mut c_void,
            creation_disposition: u32,
            flags_and_attributes: u32,
            template_file: HANDLE,
        ) -> HANDLE;
        pub fn ReadFile(
            handle: HANDLE,
            buffer: *mut c_void,
            bytes_to_read: u32,
            bytes_read: *mut u32,
            overlapped: *mut c_void,
        ) -> BOOL;
        pub fn WriteFile(
            handle: HANDLE,
            buffer: *const c_void,
            bytes_to_write: u32,
            bytes_written: *mut u32,
            overlapped: *mut c_void,
        ) -> BOOL;
        pub fn FlushFileBuffers(handle: HANDLE) -> BOOL;
        pub fn DisconnectNamedPipe(handle: HANDLE) -> BOOL;
        pub fn CloseHandle(handle: HANDLE) -> BOOL;
        pub fn DuplicateHandle(
            source_process: HANDLE,
            source_handle: HANDLE,
            target_process: HANDLE,
            target_handle: *mut HANDLE,
            desired_access: u32,
            inherit_handle: u32,
            options: u32,
        ) -> BOOL;
        pub fn GetCurrentProcess() -> HANDLE;
        pub fn WaitNamedPipeW(name: *const u16, timeout_ms: u32) -> BOOL;
    }

    pub const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;
    pub const PIPE_TYPE_BYTE: u32 = 0x0000_0000;
    pub const PIPE_READMODE_BYTE: u32 = 0x0000_0000;
    pub const PIPE_WAIT: u32 = 0x0000_0000;
    pub const PIPE_UNLIMITED_INSTANCES: u32 = 255;
    /// 64 KiB per direction, the documented maximum for named pipes.
    pub const PIPE_BUFFER_SIZE: u32 = 65536;
    pub const GENERIC_READ: u32 = 0x8000_0000;
    pub const GENERIC_WRITE: u32 = 0x4000_0000;
    pub const OPEN_EXISTING: u32 = 3;
    pub const DUPLICATE_SAME_ACCESS: u32 = 0x0000_0002;
    pub const ERROR_BROKEN_PIPE: u32 = 109;
    pub const ERROR_PIPE_BUSY: u32 = 231;
    pub const ERROR_PIPE_CONNECTED: u32 = 535;
    pub const INVALID_HANDLE_VALUE: HANDLE = -1_isize as HANDLE;

    /// Null-terminated wide string of a pipe name.
    pub fn wide(path: &std::path::Path) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }
}

#[cfg(windows)]
struct PipeListener {
    name: Vec<u16>,
    /// The next instance, created eagerly at bind time. A named pipe only
    /// exists while an instance does, so clients calling connect between
    /// bind and the first accept must still find it (Unix sockets exist as
    /// a file from bind onward).
    pending: Option<PipeConn>,
}

#[cfg(windows)]
impl PipeListener {
    fn bind(path: &std::path::Path) -> io::Result<Self> {
        if !path.to_string_lossy().starts_with(r"\\.\pipe\") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a Windows pipe path must start with \\\\.\\pipe\\",
            ));
        }
        let name = win::wide(path);
        let pending = create_instance(&name)?;
        Ok(Self {
            name,
            pending: Some(pending),
        })
    }

    /// Hand over the pre-created instance (blocking until a client opens
    /// it), then eagerly create the next one.
    fn accept(&mut self) -> io::Result<PipeConn> {
        let conn = match self.pending.take() {
            Some(conn) => conn,
            None => create_instance(&self.name)?,
        };
        let connected = unsafe { win::ConnectNamedPipe(conn.handle, std::ptr::null_mut()) } != 0;
        if !connected {
            // ERROR_PIPE_CONNECTED: a client slipped in between Create and
            // Connect. Anything else is a real failure (capture before the
            // CloseHandle in Drop clobbers the thread error).
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(win::ERROR_PIPE_CONNECTED as i32) {
                return Err(error);
            }
        }
        // Best effort: if the next instance cannot be created, the following
        // accept retries.
        self.pending = create_instance(&self.name).ok();
        Ok(conn)
    }
}

/// One listening pipe instance (server side).
#[cfg(windows)]
fn create_instance(name: &[u16]) -> io::Result<PipeConn> {
    let handle = unsafe {
        win::CreateNamedPipeW(
            name.as_ptr(),
            win::PIPE_ACCESS_DUPLEX,
            win::PIPE_TYPE_BYTE | win::PIPE_READMODE_BYTE | win::PIPE_WAIT,
            win::PIPE_UNLIMITED_INSTANCES,
            win::PIPE_BUFFER_SIZE,
            win::PIPE_BUFFER_SIZE,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle == win::INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(PipeConn { handle })
}

#[cfg(windows)]
struct PipeConn {
    handle: win::HANDLE,
}

#[cfg(windows)]
impl PipeConn {
    fn connect(path: &std::path::Path) -> io::Result<Self> {
        let name = win::wide(path);
        for _ in 0..32 {
            let handle = unsafe {
                win::CreateFileW(
                    name.as_ptr(),
                    win::GENERIC_READ | win::GENERIC_WRITE,
                    0,
                    std::ptr::null_mut(),
                    win::OPEN_EXISTING,
                    0,
                    std::ptr::null_mut(),
                )
            };
            if handle != win::INVALID_HANDLE_VALUE {
                return Ok(Self { handle });
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(win::ERROR_PIPE_BUSY as i32) {
                return Err(error);
            }
            // Busy means a server exists but is between instances; wait for
            // one to free up (200 ms each round).
            unsafe {
                win::WaitNamedPipeW(name.as_ptr(), 200);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "named pipe is busy",
        ))
    }

    /// A second handle to the same pipe, so reader and writer threads each
    /// own one (mirrors UnixStream::try_clone).
    fn duplicate(&self) -> io::Result<Self> {
        let mut target: win::HANDLE = std::ptr::null_mut();
        let ok = unsafe {
            win::DuplicateHandle(
                win::GetCurrentProcess(),
                self.handle,
                win::GetCurrentProcess(),
                &mut target,
                0,
                0,
                win::DUPLICATE_SAME_ACCESS,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle: target })
    }

    /// Best effort: flush, then drop the server-side pipe instance. Closing
    /// the handle is what actually signals EOF to the peer.
    fn disconnect(&self) {
        unsafe {
            win::FlushFileBuffers(self.handle);
            win::DisconnectNamedPipe(self.handle);
        }
    }
}

// The accept loop moves PipeConn (raw HANDLE) into per-client threads. Each
// handle is used by exactly one session thread afterwards, matching the
// ownership UnixStream::try_clone hands out.
#[cfg(windows)]
unsafe impl Send for PipeConn {}

#[cfg(windows)]
impl Drop for PipeConn {
    fn drop(&mut self) {
        unsafe {
            win::CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
impl Read for PipeConn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut read = 0u32;
        let ok = unsafe {
            win::ReadFile(
                self.handle,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let error = io::Error::last_os_error();
            // Peer closed its end: that is EOF, not a failure.
            if error.raw_os_error() == Some(win::ERROR_BROKEN_PIPE as i32) {
                return Ok(0);
            }
            return Err(error);
        }
        Ok(read as usize)
    }
}

#[cfg(windows)]
impl Write for PipeConn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut written = 0u32;
        let ok = unsafe {
            win::WriteFile(
                self.handle,
                buf.as_ptr().cast(),
                buf.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(written as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
