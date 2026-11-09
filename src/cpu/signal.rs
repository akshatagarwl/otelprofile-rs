//! SIGPROF delivery and the signal handler.
//!
//! `setitimer(ITIMER_PROF)` makes the kernel send SIGPROF to whichever thread
//! is running, 100 times per CPU second. The handler unwinds that thread
//! straight into the ring. It takes no lock and allocates nothing: the span
//! context comes from a global table (see `context`), not a thread-local,
//! because the first touch of a thread-local can allocate on macOS and in
//! `dlopen`ed libraries.

use std::ffi::{c_int, c_void};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use super::ring::{MAX_DEPTH, RawSample, Ring, THREAD_NAME_LEN};

pub(crate) static RING: Ring = Ring::new();
static ACTIVE: AtomicBool = AtomicBool::new(false);
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Libraries whose code is not unwound from a signal handler, because the
/// unwinder itself lives there (its caches are not reentrant) or because it
/// has no unwind tables (the vDSO). A sample that lands in one keeps its
/// CPU time under a single `[library]` frame.
#[cfg(target_os = "linux")]
const BLOCKLIST: &[&str] = &["libgcc", "vdso"];
#[cfg(target_os = "macos")]
const BLOCKLIST: &[&str] = &["libunwind"];

struct Segment {
    start: usize,
    end: usize,
    library: u8,
}

static BLOCKED_SEGMENTS: OnceLock<Box<[Segment]>> = OnceLock::new();
static BLOCKED_NAMES: OnceLock<Box<[String]>> = OnceLock::new();

/// The name of blocklisted library `index` (`RawSample::blocked - 1`).
pub(crate) fn blocked_library(index: u8) -> &'static str {
    BLOCKED_NAMES
        .get()
        .and_then(|names| names.get(usize::from(index)))
        .map_or("unknown", String::as_str)
}

fn find_blocked_segments() {
    use findshlibs::{Segment as _, SharedLibrary, TargetSharedLibrary};
    let mut names = Vec::new();
    let mut segments = Vec::new();
    TargetSharedLibrary::each(|library| {
        let path = library.name().to_string_lossy();
        let file = path.rsplit('/').next().unwrap_or(&path).to_string();
        if !BLOCKLIST.iter().any(|blocked| path.contains(blocked)) {
            return;
        }
        let Ok(index) = u8::try_from(names.len()) else {
            return;
        };
        names.push(if file.is_empty() {
            "vdso".to_string()
        } else {
            file
        });
        for segment in library.segments() {
            let start = segment.actual_virtual_memory_address(library).0;
            segments.push(Segment {
                start,
                end: start + segment.len(),
                library: index,
            });
        }
    });
    let _ = BLOCKED_NAMES.set(names.into_boxed_slice());
    let _ = BLOCKED_SEGMENTS.set(segments.into_boxed_slice());
}

fn blocked(pc: usize) -> Option<u8> {
    BLOCKED_SEGMENTS
        .get()?
        .iter()
        .find(|s| s.start <= pc && pc < s.end)
        .map(|s| s.library)
}

/// The interrupted instruction and stack pointer.
///
/// # Safety
/// `ucontext` must be the third argument of an `SA_SIGINFO` handler.
unsafe fn interrupted(ucontext: *mut c_void) -> (usize, usize) {
    if ucontext.is_null() {
        return (0, 0);
    }
    let ucontext = ucontext.cast::<libc::ucontext_t>();
    // SAFETY: the kernel passes a valid ucontext_t to SA_SIGINFO handlers.
    unsafe {
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            let regs = &(*ucontext).uc_mcontext.gregs;
            return (
                regs[libc::REG_RIP as usize] as usize,
                regs[libc::REG_RSP as usize] as usize,
            );
        }
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        {
            let mcontext = &(*ucontext).uc_mcontext;
            return (mcontext.pc as usize, mcontext.sp as usize);
        }
        #[cfg(target_os = "macos")]
        {
            let mcontext = (*ucontext).uc_mcontext;
            if mcontext.is_null() {
                return (0, 0);
            }
            #[cfg(target_arch = "x86_64")]
            return (
                (*mcontext).__ss.__rip as usize,
                (*mcontext).__ss.__rsp as usize,
            );
            #[cfg(target_arch = "aarch64")]
            return (
                (*mcontext).__ss.__pc as usize,
                (*mcontext).__ss.__sp as usize,
            );
        }
    }
}

fn errno_location() -> *mut c_int {
    // SAFETY: both return the calling thread's errno slot.
    unsafe {
        #[cfg(target_os = "linux")]
        return libc::__errno_location();
        #[cfg(target_os = "macos")]
        return libc::__error();
    }
}

extern "C" fn on_sigprof(_signal: c_int, _info: *mut libc::siginfo_t, ucontext: *mut c_void) {
    let errno = errno_location();
    // SAFETY: errno_location points at this thread's errno.
    let saved = unsafe { *errno };
    if ACTIVE.load(Ordering::Relaxed) {
        // SAFETY: called from the SA_SIGINFO handler with its ucontext.
        let (pc, sp) = unsafe { interrupted(ucontext) };
        let (link, labels) = super::context::current();
        RING.push(|sample| record(sample, pc, sp, link, labels));
    }
    // SAFETY: as above.
    unsafe { *errno = saved };
}

fn record(
    sample: &mut RawSample,
    pc: usize,
    sp: usize,
    link: Option<crate::model::SpanLink>,
    labels: u32,
) {
    sample.link = link;
    sample.labels = labels;
    sample.blocked = 0;
    sample.thread_name = [0; THREAD_NAME_LEN];
    // SAFETY: the buffer is THREAD_NAME_LEN bytes; for the calling thread
    // this reads its own name without locking (prctl on Linux, the pthread
    // struct on macOS).
    unsafe {
        libc::pthread_getname_np(
            libc::pthread_self(),
            sample.thread_name.as_mut_ptr().cast(),
            THREAD_NAME_LEN,
        );
    }
    if let Some(library) = blocked(pc) {
        sample.blocked = library + 1;
        sample.depth = 0;
        return;
    }
    let mut depth = 0;
    // SAFETY: this unwinds the current thread, which is what the
    // unsynchronized variant permits; nothing else unwinds this thread
    // concurrently because SIGPROF is blocked while its handler runs.
    unsafe {
        backtrace::trace_unsynchronized(|frame| {
            // The handler and the signal trampoline run on the stack below
            // the interrupted stack pointer; the interrupted frames are at
            // or above it.
            if (frame.sp() as usize) <= sp {
                return true;
            }
            let ip = frame.ip() as usize;
            // The first frame above the trampoline is the interrupted
            // function, or its caller when the unwinder skips a leaf
            // (macOS does); the pc comes first in that case.
            if depth == 0 && ip != pc && pc != 0 {
                sample.addrs[0] = pc;
                depth = 1;
            }
            sample.addrs[depth] = ip;
            depth += 1;
            depth < MAX_DEPTH
        });
    }
    if depth == 0 {
        sample.addrs[0] = pc;
        depth = 1;
    }
    sample.depth = depth as u16;
}

/// Installs the handler (once per process) and starts the timer.
pub(crate) fn start(hz: u32) -> Result<(), String> {
    if !INSTALLED.load(Ordering::Acquire) {
        find_blocked_segments();
        warm_up();
        // SAFETY: a zeroed sigaction is valid; we fill the fields we use.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction =
                on_sigprof as extern "C" fn(c_int, *mut libc::siginfo_t, *mut c_void) as usize;
            action.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            let mut previous: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(libc::SIGPROF, std::ptr::null(), &mut previous) != 0 {
                return Err(format!("sigaction: {}", std::io::Error::last_os_error()));
            }
            if previous.sa_sigaction != libc::SIG_DFL && previous.sa_sigaction != libc::SIG_IGN {
                return Err("another CPU profiler owns SIGPROF".into());
            }
            if libc::sigaction(libc::SIGPROF, &action, std::ptr::null_mut()) != 0 {
                return Err(format!("sigaction: {}", std::io::Error::last_os_error()));
            }
        }
        INSTALLED.store(true, Ordering::Release);
    }
    ACTIVE.store(true, Ordering::Relaxed);
    set_timer(1_000_000 / hz.max(1) as i64)
}

/// Stops the timer. The handler stays installed: a SIGPROF still in flight
/// would otherwise hit the default action and end the process.
pub(crate) fn stop() {
    let _ = set_timer(0);
    ACTIVE.store(false, Ordering::Relaxed);
}

fn set_timer(micros: i64) -> Result<(), String> {
    let interval = libc::timeval {
        tv_sec: (micros / 1_000_000) as libc::time_t,
        tv_usec: (micros % 1_000_000) as libc::suseconds_t,
    };
    let timer = libc::itimerval {
        it_interval: interval,
        it_value: interval,
    };
    // SAFETY: valid itimerval, null old value.
    if unsafe { libc::setitimer(libc::ITIMER_PROF, &timer, std::ptr::null_mut()) } != 0 {
        return Err(format!("setitimer: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

/// Runs the unwinder and the thread-name call once outside a signal handler
/// so lazy symbol binding and first-use setup happen here, not in it.
fn warm_up() {
    let mut name = [0u8; THREAD_NAME_LEN];
    // SAFETY: buffer of the stated length.
    unsafe {
        libc::pthread_getname_np(libc::pthread_self(), name.as_mut_ptr().cast(), name.len());
        backtrace::trace_unsynchronized(|_| true);
    }
}
