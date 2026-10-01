//! EOF 先收束身体连接；取消时解除 SDK 输出背压，避免等写锁而无法退出。

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_util::sync::WaitForCancellationFutureOwned;

use crate::CancellationToken;

pub(crate) fn guard<R, W>(
    input: R,
    output: W,
    closed: CancellationToken,
) -> (GuardedRead<R>, GuardedWrite<W>) {
    (
        GuardedRead {
            inner: input,
            closed: closed.clone(),
        },
        GuardedWrite {
            inner: output,
            cancelled: Box::pin(closed.cancelled_owned()),
        },
    )
}

pub(crate) struct GuardedRead<R> {
    inner: R,
    closed: CancellationToken,
}

impl<R: AsyncRead + Unpin> AsyncRead for GuardedRead<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let had_room = buf.remaining() > 0;
        let result = Pin::new(&mut this.inner).poll_read(cx, buf);
        match &result {
            Poll::Ready(Ok(())) if had_room && buf.filled().len() == before => {
                this.closed.cancel();
            }
            Poll::Ready(Err(_)) => this.closed.cancel(),
            _ => {}
        }
        result
    }
}

pub(crate) struct GuardedWrite<W> {
    inner: W,
    cancelled: Pin<Box<WaitForCancellationFutureOwned>>,
}

impl<W> GuardedWrite<W> {
    fn check_open(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        // 先登记取消的 waker，再进入可能无限背压的底层写操作。
        if self.cancelled.as_mut().poll(cx).is_ready() {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "MCP 输入已关闭，停止输出",
            ))
        } else {
            Ok(())
        }
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for GuardedWrite<W> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if let Err(error) = this.check_open(cx) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut this.inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Err(error) = this.check_open(cx) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Err(error) = this.check_open(cx) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::task::{Wake, Waker};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[tokio::test]
    async fn eof_cancels_only_after_buffered_input_is_read() {
        let closed = CancellationToken::new();
        let (mut peer, input) = tokio::io::duplex(8);
        peer.write_all(b"hello").await.unwrap();
        peer.shutdown().await.unwrap();
        let (mut reader, _) = guard(input, tokio::io::sink(), closed.clone());
        let mut bytes = [0; 5];
        reader.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"hello");
        assert!(!closed.is_cancelled());
        assert_eq!(reader.read(&mut []).await.unwrap(), 0);
        assert!(!closed.is_cancelled(), "零容量读取不是 EOF");
        assert_eq!(reader.read(&mut bytes).await.unwrap(), 0);
        assert!(closed.is_cancelled());
    }

    struct BrokenRead;

    impl AsyncRead for BrokenRead {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Err(io::Error::new(io::ErrorKind::ConnectionReset, "断开")))
        }
    }

    #[tokio::test]
    async fn input_error_cancels_the_connection_and_preserves_the_error() {
        let closed = CancellationToken::new();
        let (mut reader, _) = guard(BrokenRead, tokio::io::sink(), closed.clone());
        let error = reader.read(&mut [0; 1]).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
        assert!(closed.is_cancelled());
    }

    #[derive(Default)]
    struct WakeCount(AtomicUsize);

    impl Wake for WakeCount {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn cancellation_wakes_a_backpressured_write_and_releases_output_operations() {
        let closed = CancellationToken::new();
        // 保留读端而不读，填满后写端只能等待读取或取消。
        let (output, _unread) = tokio::io::duplex(1);
        let (_, mut writer) = guard(tokio::io::empty(), output, closed.clone());
        writer.write_all(b"a").await.unwrap();
        let count = Arc::new(WakeCount::default());
        let waker = Waker::from(count.clone());
        let mut cx = Context::from_waker(&waker);
        assert!(Pin::new(&mut writer).poll_write(&mut cx, b"b").is_pending());
        closed.cancel();
        assert!(count.0.load(Ordering::SeqCst) > 0, "取消必须唤醒等写的任务");
        let result = Pin::new(&mut writer).poll_write(&mut cx, b"b");
        assert!(
            matches!(result, Poll::Ready(Err(error)) if error.kind() == io::ErrorKind::BrokenPipe)
        );
        assert_eq!(
            writer.flush().await.unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_eq!(
            writer.shutdown().await.unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }
}
