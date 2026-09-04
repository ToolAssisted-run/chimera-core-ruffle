// The guest navigator: serves a movie's associated files (loadMovie, loadSound,
// getURL of a relative file) from the guest VFS the host mounted, and runs the
// load futures on a trivial in-house executor.
//
// A close port of ruffle's TestNavigatorBackend - resolve relative to file:///,
// read the file - but reading through std::fs, which the host's VFS backs, and
// with our OWN executor: futures::executor::LocalPool parks a thread and never
// polls inside the single-threaded sandbox, so a NullExecutor's run() does
// nothing here. Ours polls queued futures with a no-op waker until they stall,
// which is all a filesystem fetch (synchronous under the hood) needs, and is
// perfectly deterministic - no wall clock, no wakeups from elsewhere.
use std::borrow::Cow;
use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
use std::time::Duration;

use ruffle_core::backend::navigator::{
    async_return, create_fetch_error, ErrorResponse, NavigationMethod, NavigatorBackend,
    OwnedFuture, Request, SuccessResponse,
};
use ruffle_core::loader::Error;
use ruffle_core::socket::{SocketAction, SocketHandle};
use async_channel::{Receiver, Sender};
use encoding_rs::Encoding;
use url::{ParseError, Url};

/// The shared task queue: spawn_future pushes, the machine drains each frame.
pub type Tasks = Rc<RefCell<Vec<OwnedFuture<(), Error>>>>;

pub struct GuestNavigator {
    pub tasks: Tasks,
}

// ---- a no-op waker: our executor re-polls every pending task each frame, so a
// task never needs to signal readiness; nothing off the frame clock ever wakes.
fn noop_raw() -> RawWaker {
    fn no(_: *const ()) {}
    fn clone(_: *const ()) -> RawWaker { noop_raw() }
    static V: RawWakerVTable = RawWakerVTable::new(clone, no, no, no);
    RawWaker::new(core::ptr::null(), &V)
}
fn noop_waker() -> Waker {
    unsafe { Waker::from_raw(noop_raw()) }
}

/// Poll every queued task once; keep the ones still pending. Called each frame
/// after the movie has run, exactly where ruffle's runner drains its executor.
pub fn run_tasks(tasks: &Tasks) {
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    // take the current batch; tasks spawned WHILE polling land in a fresh Vec
    // and run next frame, which matches how a real frame-driven loader behaves.
    let batch: Vec<OwnedFuture<(), Error>> = std::mem::take(&mut *tasks.borrow_mut());
    let mut still_pending = Vec::new();
    for mut fut in batch {
        if fut.as_mut().poll(&mut cx).is_pending() {
            still_pending.push(fut);
        }
    }
    tasks.borrow_mut().extend(still_pending);
}

struct GuestResponse {
    url: String,
    body: Vec<u8>,
}
impl SuccessResponse for GuestResponse {
    fn url(&self) -> Cow<'_, str> { Cow::Borrowed(&self.url) }
    fn set_url(&mut self, url: String) { self.url = url; }
    fn body(self: Box<Self>) -> OwnedFuture<Vec<u8>, Error> {
        Box::pin(async move { Ok(self.body) })
    }
    fn text_encoding(&self) -> Option<&'static Encoding> { None }
    fn status(&self) -> u16 { 0 }
    fn redirected(&self) -> bool { false }
    fn next_chunk(&mut self) -> OwnedFuture<Option<Vec<u8>>, Error> {
        Box::pin(async move { Ok(None) })
    }
    fn expected_length(&self) -> Result<Option<u64>, Error> { Ok(Some(self.body.len() as u64)) }
}

/// file:///foo/bar.swf -> "foo/bar.swf"; a spoofed http://host/foo -> "host/foo".
/// Flat, matching how the host mounts associated files by name.
fn url_to_vfs_path(url: &Url) -> String {
    let mut path = String::new();
    if url.scheme() != "file" {
        if let Some(host) = url.host_str() {
            path.push_str(host);
        }
    }
    let p = url.path().trim_start_matches('/');
    let decoded = percent_encoding::percent_decode_str(p).decode_utf8_lossy().into_owned();
    if !path.is_empty() && !decoded.is_empty() {
        path.push('/');
    }
    path.push_str(&decoded);
    path
}

impl NavigatorBackend for GuestNavigator {
    fn navigate_to_url(
        &self,
        _url: &str,
        _target: &str,
        _vars_method: Option<(NavigationMethod, indexmap::IndexMap<String, String>)>,
    ) {
    }

    fn fetch(&self, request: Request) -> OwnedFuture<Box<dyn SuccessResponse>, ErrorResponse> {
        let url = match self.resolve_url(request.url()) {
            Ok(u) => u,
            Err(e) => return async_return(Err(create_fetch_error(request.url(), e))),
        };
        let path = url_to_vfs_path(&url);
        Box::pin(async move {
            // The host mounts associated files FLAT, by basename - that is the
            // whole scheme. A movie rarely asks that way: it asks relative to
            // itself, and ruffle resolves that against the movie's own URL, so a
            // spoofed URL turns "levels.xml" into "host/levels.xml", and a movie
            // kept in a subdirectory asks for "assets/levels.xml". Neither names a
            // mounted file, though the file is right there. So: try what was
            // asked for, then what it is called.
            let mut read = std::fs::read(&path);
            if read.is_err() {
                if let Some((_, base)) = path.rsplit_once('/') {
                    if !base.is_empty() {
                        read = std::fs::read(base);
                    }
                }
            }
            match read {
                Ok(body) => {
                    let r: Box<dyn SuccessResponse> = Box::new(GuestResponse { url: url.to_string(), body });
                    Ok(r)
                }
                Err(e) => {
                    // A movie asking for a file it was not given is the single
                    // most common way a real game half-works, and it used to fail
                    // in silence. Say what was asked for AND the name it was
                    // looked up under: the host mounts associated files flat, by
                    // basename, so a movie whose URL was spoofed asks for
                    // "host/levels.xml" while the file sits there as "levels.xml".
                    eprintln!(
                        "ruffle: could not load '{}' - looked for '{}' among the files mounted beside the movie ({})",
                        url, path, e
                    );
                    Err(ErrorResponse { url: url.to_string(), error: Error::FetchError(e.to_string()) })
                }
            }
        })
    }

    fn resolve_url(&self, url: &str) -> Result<Url, ParseError> {
        let mut base = Url::parse("file:///").unwrap();
        base.path_segments_mut().unwrap().push("");
        match base.join(url) {
            Ok(u) => Ok(u),
            Err(_) => Url::parse(url),
        }
    }

    fn spawn_future(&mut self, future: OwnedFuture<(), Error>) {
        self.tasks.borrow_mut().push(future);
    }

    fn pre_process_url(&self, url: Url) -> Url { url }

    fn connect_socket(
        &mut self,
        _host: String,
        _port: u16,
        _timeout: Duration,
        _handle: SocketHandle,
        _receiver: Receiver<Vec<u8>>,
        _sender: Sender<SocketAction>,
    ) {
        // real network, non-deterministic: unsupported
    }
}

// keep Future/Pin used even if the trait fns above are all that reference them
#[allow(dead_code)]
type _F = Pin<Box<dyn Future<Output = ()>>>;
