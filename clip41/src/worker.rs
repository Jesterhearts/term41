//! Keep platform clipboard calls from blocking terminal and window threads
//! forever.

use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::SyncSender;
use std::thread;
use std::time::Duration;

use arboard::Clipboard as ArboardClipboard;

use super::ClipboardImage;
use super::ClipboardKind;
use super::get_real;
use super::set_real;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(1);

type Reply<T> = SyncSender<Result<T, arboard::Error>>;

enum Request {
    GetText(ClipboardKind, Reply<String>),
    SetText(ClipboardKind, String, Reply<()>),
    GetImage(Reply<ClipboardImage>),
}

pub(super) struct ClipboardWorker {
    requests: SyncSender<Request>,
    // A timed-out platform call still owns the worker. Require its completion
    // before accepting another request, especially a clipboard write that
    // could otherwise run long after the user has copied something else.
    idle: Receiver<()>,
}

fn worker_channels() -> (ClipboardWorker, Receiver<Request>, SyncSender<()>) {
    let (requests, receiver) = mpsc::sync_channel(1);
    let (idle_tx, idle) = mpsc::sync_channel(1);
    (ClipboardWorker { requests, idle }, receiver, idle_tx)
}

pub(super) fn spawn() -> Result<ClipboardWorker, String> {
    let (worker, requests, idle) = worker_channels();
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("clipboard".into())
        .spawn(move || {
            let clipboard = match ArboardClipboard::new() {
                Ok(clipboard) => clipboard,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
            if idle.send(()).is_err() || ready_tx.send(Ok(())).is_err() {
                return;
            }
            run(clipboard, requests, idle);
        })
        .map_err(|error| format!("could not start clipboard worker: {error}"))?;
    receive(ready_rx, REQUEST_TIMEOUT)?;
    Ok(worker)
}

fn run(
    mut clipboard: ArboardClipboard,
    requests: Receiver<Request>,
    idle: SyncSender<()>,
) {
    while let Ok(request) = requests.recv() {
        match request {
            Request::GetText(kind, reply) => {
                let result = get_real(&mut clipboard, kind);
                complete(&idle, reply, result);
            }
            Request::SetText(kind, text, reply) => {
                let result = set_real(&mut clipboard, kind, &text);
                complete(&idle, reply, result);
            }
            Request::GetImage(reply) => {
                let result = clipboard.get_image().map(|image| ClipboardImage {
                    width: image.width as u32,
                    height: image.height as u32,
                    rgba: image.bytes.into_owned(),
                });
                complete(&idle, reply, result);
            }
        }
    }
}

fn complete<T>(
    idle: &SyncSender<()>,
    reply: Reply<T>,
    result: Result<T, arboard::Error>,
) {
    // Publish availability before the response so the next call can start as
    // soon as the caller receives its result. Neither channel can be full:
    // each request consumes the idle token and has its own one-shot reply.
    let _ = idle.send(());
    let _ = reply.send(result);
}

fn send(
    worker: &mut ClipboardWorker,
    request: Request,
) -> Result<(), String> {
    worker.idle.try_recv().map_err(|error| match error {
        mpsc::TryRecvError::Empty => "previous clipboard request is still running".to_owned(),
        mpsc::TryRecvError::Disconnected => "clipboard worker stopped".to_owned(),
    })?;
    worker
        .requests
        .try_send(request)
        .map_err(|_| "clipboard worker stopped".to_owned())
}

fn receive<T>(
    reply: Receiver<Result<T, arboard::Error>>,
    timeout: Duration,
) -> Result<T, String> {
    reply
        .recv_timeout(timeout)
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => "clipboard request timed out".to_owned(),
            mpsc::RecvTimeoutError::Disconnected => "clipboard worker stopped".to_owned(),
        })?
        .map_err(|error| error.to_string())
}

pub(super) fn get_text(
    worker: &mut ClipboardWorker,
    kind: ClipboardKind,
) -> Result<String, String> {
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    send(worker, Request::GetText(kind, reply_tx))?;
    receive(reply_rx, REQUEST_TIMEOUT)
}

pub(super) fn set_text(
    worker: &mut ClipboardWorker,
    kind: ClipboardKind,
    text: &str,
) -> Result<(), String> {
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    send(worker, Request::SetText(kind, text.to_owned(), reply_tx))?;
    receive(reply_rx, REQUEST_TIMEOUT)
}

pub(super) fn get_image(worker: &mut ClipboardWorker) -> Result<ClipboardImage, String> {
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    send(worker, Request::GetImage(reply_tx))?;
    receive(reply_rx, REQUEST_TIMEOUT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stalled_read_times_out_without_queueing_later_requests() {
        let (mut worker, requests, idle) = worker_channels();
        idle.send(()).unwrap();
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        send(
            &mut worker,
            Request::GetText(ClipboardKind::Clipboard, reply_tx),
        )
        .unwrap();
        let Request::GetText(_, stalled_reply) = requests.recv().unwrap() else {
            panic!("expected text request");
        };

        assert_eq!(
            receive(reply_rx, Duration::from_millis(10)).unwrap_err(),
            "clipboard request timed out"
        );
        assert_eq!(
            get_text(&mut worker, ClipboardKind::Primary).unwrap_err(),
            "previous clipboard request is still running"
        );
        assert_eq!(
            set_text(&mut worker, ClipboardKind::Clipboard, "new copy").unwrap_err(),
            "previous clipboard request is still running"
        );
        assert!(matches!(
            requests.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        // A late reply must neither block the worker nor become the next paste.
        complete(&idle, stalled_reply, Ok("stale text".into()));
        let server = thread::spawn(move || {
            let Request::GetText(kind, reply) = requests.recv().unwrap() else {
                panic!("expected text request");
            };
            assert_eq!(kind, ClipboardKind::Primary);
            complete(&idle, reply, Ok("current selection".into()));
        });
        assert_eq!(
            get_text(&mut worker, ClipboardKind::Primary).unwrap(),
            "current selection"
        );
        server.join().unwrap();
    }

    #[test]
    fn completed_write_is_followed_by_a_fresh_read() {
        let (mut worker, requests, idle) = worker_channels();
        idle.send(()).unwrap();
        let server = thread::spawn(move || {
            let Request::SetText(kind, text, reply) = requests.recv().unwrap() else {
                panic!("expected write request");
            };
            assert_eq!(kind, ClipboardKind::Clipboard);
            assert_eq!(text, "copied text");
            complete(&idle, reply, Ok(()));
            let Request::GetText(kind, reply) = requests.recv().unwrap() else {
                panic!("expected text request");
            };
            assert_eq!(kind, ClipboardKind::Clipboard);
            complete(&idle, reply, Ok("changed by another application".into()));
        });
        set_text(&mut worker, ClipboardKind::Clipboard, "copied text").unwrap();
        assert_eq!(
            get_text(&mut worker, ClipboardKind::Clipboard).unwrap(),
            "changed by another application"
        );
        server.join().unwrap();
    }

    #[test]
    fn backend_failure_leaves_worker_available() {
        let (mut worker, requests, idle) = worker_channels();
        idle.send(()).unwrap();
        let server = thread::spawn(move || {
            let Request::GetImage(reply) = requests.recv().unwrap() else {
                panic!("expected image request");
            };
            complete(&idle, reply, Err(arboard::Error::ContentNotAvailable));
            let Request::GetText(_, reply) = requests.recv().unwrap() else {
                panic!("expected text request");
            };
            complete(&idle, reply, Ok("text only".into()));
        });
        assert!(get_image(&mut worker).is_err());
        assert_eq!(
            get_text(&mut worker, ClipboardKind::Clipboard).unwrap(),
            "text only"
        );
        server.join().unwrap();
    }

    #[test]
    fn stopped_worker_returns_an_error() {
        let (mut worker, requests, idle) = worker_channels();
        drop(requests);
        drop(idle);
        assert_eq!(
            get_text(&mut worker, ClipboardKind::Clipboard).unwrap_err(),
            "clipboard worker stopped"
        );
    }
}
